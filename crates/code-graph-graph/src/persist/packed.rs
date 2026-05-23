//! v6 cache: path-and-name interned, still JSON on the wire.
//!
//! Replaces v5's full-strings-everywhere [`GraphCache`](super::GraphCache)
//! with a shape that stores each path and each name **once** in an
//! interner table, then references them by `u32` everywhere else. On
//! UE/LLVM-scale codebases path strings repeat tens of millions of times
//! across `nodes`/`adj`/`radj`/`files`/`includes`/`mtimes` plus inside
//! every embedded `SymbolId` (`<path>:<name>`); the interning alone is
//! expected to shrink the on-disk cache by ~3-5×.
//!
//! Per the PackedCache design's Phase B/C split (see
//! `.plans/Designs/PackedCache/README.md`), the full columnar CSR
//! layout from the design's Schema section is **not** part of Phase B
//! — it moves to Phase C alongside rkyv, where the binary format's
//! native vector layout makes CSR structurally cheaper.
//!
//! # Reserved sentinels
//!
//! - `PathId(0)`: "no path" (used for bare-token unresolved edge
//!   targets like `Ok`, `printf`).
//! - `NameId(0)`: reserved; the encoder never assigns it. `0` in any
//!   `Option<u32>` field (`namespace`, `parent`) is interpreted as
//!   "absent" at decode time — equivalent to the wire-form `None`.

use crate::graph::{EdgeEntry, FileEntry, Graph, IncludeEntry, Node};
use code_graph_core::{symbol_id, EdgeKind, Language, Symbol, SymbolId, SymbolKind};
use lasso::{Key, Rodeo, Spur};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Current packed cache schema version. Bumped from v5 (`5`) — every
/// existing v5 JSON cache fails the version check on load and triggers
/// the documented silent-re-index path.
pub const CACHE_VERSION: u32 = 6;

/// Generator stamp written into the cache for diagnostic visibility.
const GENERATOR: &str = "code-graph-graph (rust, v6 interned)";

// ---------------------------------------------------------------------------
// Wire-form DTO
// ---------------------------------------------------------------------------

/// On-disk shape. Keys in every map are `u32` ids resolved via
/// [`paths`](Self::paths) / [`names`](Self::names).
///
/// **Wire key encoding.** `serde_json` requires map keys to be strings.
/// We accept the serialization of `u32` keys as their decimal-digit
/// strings — natively supported by serde — keeping the v6 file
/// trivially inspectable with `jq`. The `u32` round-trips losslessly.
#[derive(Serialize, Deserialize)]
pub(crate) struct PackedCacheV6 {
    pub version: u32,
    pub generator: String,
    #[serde(default)]
    pub last_sweep_at: u64,

    /// Path interner table. `paths[i]` is the path whose `PathId` is
    /// `i as u32 + 1` (since `PathId(0)` is the "no path" sentinel).
    pub paths: Vec<PathBuf>,

    /// Name interner table. `names[i]` is the string whose `NameId` is
    /// `i as u32 + 1`. Holds: symbol names, namespaces, parents, AND
    /// the full interned `SymbolId` strings used as map keys.
    pub names: Vec<String>,

    /// `node_id (NameId of full SymbolId)` → `PackedSymbol`.
    pub nodes: HashMap<u32, PackedSymbol>,

    /// `from_id (NameId)` → outgoing edges.
    pub adj: HashMap<u32, Vec<PackedEdge>>,

    /// `to_id (NameId)` → incoming edges. May contain `from_id`s that
    /// are not in `nodes` (bare-token unresolved targets).
    pub radj: HashMap<u32, Vec<PackedEdge>>,

    /// `file_path_id (PathId)` → file metadata.
    pub files: HashMap<u32, PackedFile>,

    /// `file_path_id (PathId)` → include list.
    pub includes: HashMap<u32, Vec<PackedInclude>>,

    /// `file_path_id (PathId)` → mtime nanos. Files whose mtime can't
    /// be read are recorded as `0` so `stale_paths` re-flags them.
    pub mtimes: HashMap<u32, u64>,
}

/// Per-symbol record. Layout mirrors [`Symbol`] but every string is an
/// interner reference. Reserved-zero convention used for `namespace` /
/// `parent` (absent → omitted from wire via `skip_serializing_if`).
#[derive(Serialize, Deserialize)]
pub(crate) struct PackedSymbol {
    pub name: u32,
    pub kind: SymbolKind,
    /// `0` when the symbol has no associated file path (a synthetic
    /// node for an unresolved edge target — does not normally occur for
    /// nodes built from `Symbol`s, but we preserve the slot for
    /// schematic symmetry with the radj key space).
    pub path: u32,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    /// Inline per [Decision 9](../../.plans/Designs/PackedCache/README.md#decision-9-symbolsignature-handling)
    /// — signatures rarely repeat verbatim, so interning hurts more than
    /// it helps.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
    pub language: Language,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PackedEdge {
    pub target: u32, // NameId — the OTHER endpoint's interned SymbolId
    pub kind: EdgeKind,
    pub file: u32, // PathId where the edge was declared
    pub line: u32,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PackedFile {
    pub language: Language,
    pub symbol_ids: Vec<u32>, // NameId values
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PackedInclude {
    pub path: u32, // PathId of the included file
    pub line: u32,
}

// ---------------------------------------------------------------------------
// Build-side interners
// ---------------------------------------------------------------------------

/// Encoder-side string interner. Thin wrapper over [`lasso::Rodeo`] that
/// also tracks insertion order for deterministic serialization.
///
/// `intern` is mostly delegated to `Rodeo::get_or_intern` (returns a
/// stable `Spur`), but the visible `u32` id is `spur.into_usize() + 1`
/// so that `0` is reserved (mirrors `PathId` convention). The
/// `into_vec` finalize step returns the strings ordered by `u32` id —
/// which IS insertion order, since lasso assigns sequentially.
struct EncodingStringInterner {
    rodeo: Rodeo,
    order: Vec<Spur>,
}

impl EncodingStringInterner {
    fn new() -> Self {
        Self {
            rodeo: Rodeo::default(),
            order: Vec::new(),
        }
    }

    /// Intern `s` if first-seen; return its `u32` id (1-based).
    fn intern(&mut self, s: &str) -> u32 {
        let len_before = self.rodeo.len();
        let spur = self.rodeo.get_or_intern(s);
        if self.rodeo.len() != len_before {
            // First time we saw this string.
            self.order.push(spur);
        }
        Self::spur_to_id(spur)
    }

    /// Same as [`intern`](Self::intern) but returns `None` for empty input.
    fn intern_opt(&mut self, s: &str) -> Option<u32> {
        if s.is_empty() {
            None
        } else {
            Some(self.intern(s))
        }
    }

    /// Finalize: emit the `names` vec in id order.
    fn into_vec(self) -> Vec<String> {
        let Self { rodeo, order } = self;
        let resolver = rodeo.into_resolver();
        order
            .into_iter()
            .map(|spur| resolver.resolve(&spur).to_string())
            .collect()
    }

    fn spur_to_id(spur: Spur) -> u32 {
        // Spur::into_usize is 0-based; we shift to 1-based so that
        // u32 0 stays reserved.
        u32::try_from(spur.into_usize() + 1).expect("name count fits in u32")
    }
}

/// Encoder-side path interner. Backed by [`code_graph_path_trie::PathInterner`]
/// so that future Phase D work using the same interner over the live
/// graph reuses the same data structure. Adds an explicit `paths()`
/// finalizer that returns paths in id-assignment order.
struct EncodingPathInterner {
    inner: code_graph_path_trie::PathInterner,
    /// Insertion-ordered Vec of paths; index = `PathId - 1`.
    order: Vec<PathBuf>,
}

impl EncodingPathInterner {
    fn new() -> Self {
        Self {
            inner: code_graph_path_trie::PathInterner::new(),
            order: Vec::new(),
        }
    }

    fn intern(&mut self, p: &Path) -> u32 {
        let len_before = self.inner.len();
        let id = self.inner.intern(p);
        if self.inner.len() != len_before {
            self.order.push(p.to_path_buf());
        }
        id.get()
    }

    /// Intern but treat empty / sentinel paths as `0`.
    fn intern_str_path(&mut self, s: &str) -> u32 {
        if s.is_empty() {
            0
        } else {
            self.intern(Path::new(s))
        }
    }

    fn into_vec(self) -> Vec<PathBuf> {
        self.order
    }
}

// ---------------------------------------------------------------------------
// Encode: Graph -> PackedCacheV6
// ---------------------------------------------------------------------------

/// Build a v6 packed cache from a live [`Graph`] + freshly-stat'd mtimes.
///
/// Interner assignment is deterministic given the (deterministic) iteration
/// order over the source maps below. The encoder pre-sorts the live
/// `HashMap` keys before interning so that two saves of the same graph
/// produce byte-identical output (matches design Goal 4).
pub(crate) fn encode(graph: &Graph, last_sweep_at: u64) -> PackedCacheV6 {
    let mut paths = EncodingPathInterner::new();
    let mut names = EncodingStringInterner::new();

    // ----- Stat mtimes for every indexed file. -----
    // Reads disk, so done first to fail fast if files disappeared.
    let mut mtimes_raw: HashMap<PathBuf, u64> = HashMap::with_capacity(graph.files.len());
    for path in graph.files.keys() {
        mtimes_raw.insert(path.clone(), super::mtime_nanos(path).unwrap_or(0));
    }

    // ----- Sort keysets for byte-stable ordering. -----
    let mut sorted_node_ids: Vec<&SymbolId> = graph.nodes.keys().collect();
    sorted_node_ids.sort();

    let mut sorted_file_paths: Vec<&PathBuf> = graph.files.keys().collect();
    sorted_file_paths.sort();

    let mut sorted_include_keys: Vec<&PathBuf> = graph.includes.keys().collect();
    sorted_include_keys.sort();

    let mut sorted_adj_keys: Vec<&SymbolId> = graph.adj.keys().collect();
    sorted_adj_keys.sort();

    let mut sorted_radj_keys: Vec<&SymbolId> = graph.radj.keys().collect();
    sorted_radj_keys.sort();

    // ----- Encode nodes. -----
    let mut packed_nodes: HashMap<u32, PackedSymbol> = HashMap::with_capacity(graph.nodes.len());
    for sid in &sorted_node_ids {
        let node = &graph.nodes[*sid];
        let sym = &node.symbol;
        let id = names.intern(sid);
        let path_id = paths.intern_str_path(&sym.file);
        let packed = PackedSymbol {
            name: names.intern(&sym.name),
            kind: sym.kind,
            path: path_id,
            line: sym.line,
            column: sym.column,
            end_line: sym.end_line,
            signature: sym.signature.clone(),
            namespace: names.intern_opt(&sym.namespace),
            parent: names.intern_opt(&sym.parent),
            language: sym.language,
        };
        packed_nodes.insert(id, packed);
    }

    // ----- Encode adjacency maps. -----
    let packed_adj = encode_edge_map(&sorted_adj_keys, &graph.adj, &mut names, &mut paths);
    let packed_radj = encode_edge_map(&sorted_radj_keys, &graph.radj, &mut names, &mut paths);

    // ----- Encode files map (and capture per-file SymbolId references). -----
    let mut packed_files: HashMap<u32, PackedFile> = HashMap::with_capacity(graph.files.len());
    for path in &sorted_file_paths {
        let fe = &graph.files[*path];
        let path_id = paths.intern(path);
        // Preserve insertion order: the live graph's `FileEntry.symbol_ids`
        // reflects `merge_file_graph` push order and round-trip tests
        // pin on it. Determinism is inherited from the live graph, not
        // enforced here.
        let symbol_ids: Vec<u32> = fe.symbol_ids.iter().map(|sid| names.intern(sid)).collect();
        let packed = PackedFile {
            language: fe.language,
            symbol_ids,
        };
        packed_files.insert(path_id, packed);
    }

    // ----- Encode includes map. -----
    let mut packed_includes: HashMap<u32, Vec<PackedInclude>> =
        HashMap::with_capacity(graph.includes.len());
    for path in &sorted_include_keys {
        let entries = &graph.includes[*path];
        let path_id = paths.intern(path);
        // Preserve order — round-trip must be a value-identity transform.
        let packed_entries: Vec<PackedInclude> = entries
            .iter()
            .map(|ie| PackedInclude {
                path: paths.intern(&ie.path),
                line: ie.line,
            })
            .collect();
        packed_includes.insert(path_id, packed_entries);
    }

    // ----- Encode mtimes map. -----
    let mut packed_mtimes: HashMap<u32, u64> = HashMap::with_capacity(mtimes_raw.len());
    for (path, nanos) in mtimes_raw {
        let path_id = paths.intern(&path);
        packed_mtimes.insert(path_id, nanos);
    }

    PackedCacheV6 {
        version: CACHE_VERSION,
        generator: GENERATOR.to_string(),
        last_sweep_at,
        paths: paths.into_vec(),
        names: names.into_vec(),
        nodes: packed_nodes,
        adj: packed_adj,
        radj: packed_radj,
        files: packed_files,
        includes: packed_includes,
        mtimes: packed_mtimes,
    }
}

fn encode_edge_map(
    sorted_keys: &[&SymbolId],
    map: &HashMap<SymbolId, Vec<EdgeEntry>>,
    names: &mut EncodingStringInterner,
    paths: &mut EncodingPathInterner,
) -> HashMap<u32, Vec<PackedEdge>> {
    let mut out: HashMap<u32, Vec<PackedEdge>> = HashMap::with_capacity(map.len());
    for sid in sorted_keys {
        let entries = &map[*sid];
        let key_id = names.intern(sid);
        // Preserve insertion order — round-trip identity matters more
        // than wire-stability across nondeterministic input.
        let packed_entries: Vec<PackedEdge> = entries
            .iter()
            .map(|e| PackedEdge {
                target: names.intern(&e.target),
                kind: e.kind,
                file: paths.intern_str_path(e.file.to_str().unwrap_or("")),
                line: e.line,
            })
            .collect();
        out.insert(key_id, packed_entries);
    }
    out
}

// ---------------------------------------------------------------------------
// Decode: PackedCacheV6 -> Graph
// ---------------------------------------------------------------------------

/// Decoded parts ready to populate a [`Graph`]. Returned as a struct
/// (not assigned in place) so [`Graph::load`] keeps full control over
/// the self-assignment sequence.
pub(crate) struct DecodedParts {
    pub nodes: HashMap<SymbolId, Node>,
    pub adj: HashMap<SymbolId, Vec<EdgeEntry>>,
    pub radj: HashMap<SymbolId, Vec<EdgeEntry>>,
    pub files: HashMap<PathBuf, FileEntry>,
    pub includes: HashMap<PathBuf, Vec<IncludeEntry>>,
}

/// Reconstruct a [`Graph`]'s component maps from a v6 packed cache.
///
/// Returns an `Err` if the cache references an id that doesn't resolve
/// (corrupt cache); this maps to `Ok(false)` at the [`Graph::load`]
/// boundary so the caller silently re-indexes.
pub(crate) fn decode(cache: PackedCacheV6) -> Result<DecodedParts, DecodeError> {
    let resolver = Resolver::new(&cache)?;

    // Rebuild nodes.
    let mut nodes: HashMap<SymbolId, Node> = HashMap::with_capacity(cache.nodes.len());
    for (id, packed) in &cache.nodes {
        let symbol_id_str = resolver.name(*id)?.to_string();
        let symbol = Symbol {
            name: resolver.name(packed.name)?.to_string(),
            kind: packed.kind,
            file: resolver.path_to_string(packed.path),
            line: packed.line,
            column: packed.column,
            end_line: packed.end_line,
            signature: packed.signature.clone(),
            namespace: packed
                .namespace
                .map(|id| resolver.name(id).map(str::to_string))
                .transpose()?
                .unwrap_or_default(),
            parent: packed
                .parent
                .map(|id| resolver.name(id).map(str::to_string))
                .transpose()?
                .unwrap_or_default(),
            language: packed.language,
        };
        // Sanity check: derived symbol_id matches what was stored. If
        // they diverge, the cache was written by a different version of
        // `symbol_id` — treat as corruption.
        let derived = symbol_id(&symbol);
        if derived != symbol_id_str {
            return Err(DecodeError::InconsistentSymbolId {
                stored: symbol_id_str,
                derived,
            });
        }
        nodes.insert(symbol_id_str, Node { symbol });
    }

    let adj = decode_edge_map(&cache.adj, &resolver)?;
    let radj = decode_edge_map(&cache.radj, &resolver)?;

    // Rebuild files map.
    let mut files: HashMap<PathBuf, FileEntry> = HashMap::with_capacity(cache.files.len());
    for (path_id, packed) in &cache.files {
        let path = resolver.path(*path_id)?.to_path_buf();
        let symbol_ids: Result<Vec<SymbolId>, DecodeError> = packed
            .symbol_ids
            .iter()
            .map(|nid| resolver.name(*nid).map(str::to_string))
            .collect();
        files.insert(
            path,
            FileEntry {
                language: packed.language,
                symbol_ids: symbol_ids?,
            },
        );
    }

    // Rebuild includes map.
    let mut includes: HashMap<PathBuf, Vec<IncludeEntry>> =
        HashMap::with_capacity(cache.includes.len());
    for (path_id, packed_entries) in &cache.includes {
        let path = resolver.path(*path_id)?.to_path_buf();
        let entries: Result<Vec<IncludeEntry>, DecodeError> = packed_entries
            .iter()
            .map(|pe| {
                Ok(IncludeEntry {
                    path: resolver.path(pe.path)?.to_path_buf(),
                    line: pe.line,
                })
            })
            .collect();
        includes.insert(path, entries?);
    }

    Ok(DecodedParts {
        nodes,
        adj,
        radj,
        files,
        includes,
    })
}

fn decode_edge_map(
    map: &HashMap<u32, Vec<PackedEdge>>,
    resolver: &Resolver,
) -> Result<HashMap<SymbolId, Vec<EdgeEntry>>, DecodeError> {
    let mut out: HashMap<SymbolId, Vec<EdgeEntry>> = HashMap::with_capacity(map.len());
    for (key_id, packed_entries) in map {
        let key = resolver.name(*key_id)?.to_string();
        let entries: Result<Vec<EdgeEntry>, DecodeError> = packed_entries
            .iter()
            .map(|pe| {
                Ok(EdgeEntry {
                    target: resolver.name(pe.target)?.to_string(),
                    kind: pe.kind,
                    file: PathBuf::from(resolver.path_to_string(pe.file)),
                    line: pe.line,
                })
            })
            .collect();
        out.insert(key, entries?);
    }
    Ok(out)
}

/// Errors raised by [`decode`]. Surfaces to [`Graph::load`] which maps
/// them to `Ok(false)` (silent re-index).
#[derive(Debug, thiserror::Error)]
pub(crate) enum DecodeError {
    #[error("path id {0} out of range")]
    PathOutOfRange(u32),
    #[error("name id {0} out of range")]
    NameOutOfRange(u32),
    #[error("stored symbol_id {stored:?} disagrees with derived {derived:?}")]
    InconsistentSymbolId { stored: String, derived: String },
}

/// Borrowed view of the cache's interner tables. Both lookups are
/// O(1) (Vec indexing); the `Resolver` exists so error reporting is
/// uniform.
struct Resolver<'a> {
    paths: &'a [PathBuf],
    names: &'a [String],
}

impl<'a> Resolver<'a> {
    fn new(cache: &'a PackedCacheV6) -> Result<Self, DecodeError> {
        // No structural validation needed up-front; per-id lookups
        // surface OutOfRange on first miss.
        Ok(Self {
            paths: &cache.paths,
            names: &cache.names,
        })
    }

    /// Resolve `PathId` to `&Path`. `0` is invalid in this context —
    /// callers that allow the sentinel use [`path_to_string`] which
    /// returns `""`.
    fn path(&self, id: u32) -> Result<&Path, DecodeError> {
        if id == 0 {
            return Err(DecodeError::PathOutOfRange(0));
        }
        self.paths
            .get(id as usize - 1)
            .map(PathBuf::as_path)
            .ok_or(DecodeError::PathOutOfRange(id))
    }

    /// Resolve `PathId` to a string for `Symbol.file` / `EdgeEntry.file`
    /// shape preservation. `0` resolves to `""`.
    fn path_to_string(&self, id: u32) -> String {
        if id == 0 {
            return String::new();
        }
        self.paths
            .get(id as usize - 1)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn name(&self, id: u32) -> Result<&str, DecodeError> {
        if id == 0 {
            return Err(DecodeError::NameOutOfRange(0));
        }
        self.names
            .get(id as usize - 1)
            .map(String::as_str)
            .ok_or(DecodeError::NameOutOfRange(id))
    }
}

// ---------------------------------------------------------------------------
// stale_paths slim DTO
// ---------------------------------------------------------------------------

/// Minimal-deserialization DTO for [`super::stale_paths`] under v6.
///
/// Mirrors the v5 [`StalePathsCache`](super::StalePathsCache) trick:
/// serde silently skips every field NOT named here, so loading this
/// type costs only `paths` + `mtimes` heap rather than the full graph
/// (which on multi-million-symbol caches is the difference between
/// tens of MB and several GB of allocations for one query).
///
/// Returns paths by re-resolving each `u32` PathId against the `paths`
/// vec. See [`super::stale_paths`] for the call site.
#[derive(Deserialize)]
pub(crate) struct StalePathsCacheV6 {
    pub paths: Vec<PathBuf>,
    pub mtimes: HashMap<u32, u64>,
}

impl StalePathsCacheV6 {
    /// Yield `(path, mtime_nanos)` pairs by joining the two fields.
    /// Out-of-range PathIds are silently dropped (corrupt cache).
    pub fn iter_resolved(&self) -> impl Iterator<Item = (PathBuf, u64)> + '_ {
        self.mtimes.iter().filter_map(|(id, nanos)| {
            if *id == 0 {
                return None;
            }
            self.paths
                .get(*id as usize - 1)
                .map(|p| (p.clone(), *nanos))
        })
    }
}
