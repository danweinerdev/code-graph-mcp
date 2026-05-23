//! v7 cache: path-and-name interned, **rkyv binary format with
//! zero-copy mmap load**.
//!
//! Phase C of the PackedCache plan. Same interning shape as v6 (Phase
//! B's JSON wire), but the on-disk format is now a rkyv archive
//! prepended by an 8-byte header (4-byte endian probe + 4-byte
//! version). Loading mmaps the file, validates the header, runs
//! `rkyv::access` (bytecheck), and walks the resulting
//! `Archived<PackedCacheV6>` directly to rebuild the live graph's
//! `HashMap`s — no allocation per byte the way JSON parse required.
//!
//! Why "V6" not "V7" in the type name: the SCHEMA is unchanged from
//! Phase B (still `paths` + `names` + the same interned map shapes);
//! only the FORMAT (rkyv vs JSON) bumped. The `CACHE_VERSION`
//! constant moved 6 → 7 because the format change is wire-breaking,
//! but the in-code type name stays stable for ergonomics.
//!
//! Per the PackedCache design's Phase B/C split (see
//! `.plans/Designs/PackedCache/README.md`), the full columnar CSR
//! restructuring deferred from Phase B is also deferred from this
//! Phase C: HashMap-shaped maps work with rkyv's `ArchivedHashMap` and
//! the dedup win is already captured by interning. CSR would only pay
//! off if benches show HashMap-archive build cost is too slow on
//! UE/LLVM-scale graphs — measure first, restructure later.
//!
//! # Reserved sentinels
//!
//! - `PathId(0)`: "no path" (used for bare-token unresolved edge
//!   targets like `Ok`, `printf`).
//! - `NameId(0)`: reserved; the encoder never assigns it. `0` in any
//!   `Option<u32>` field (`namespace`, `parent`) is interpreted as
//!   "absent" at decode time.

use crate::graph::{EdgeEntry, FileEntry, Graph, IncludeEntry, Node};
use code_graph_core::{symbol_id, EdgeKind, Language, Symbol, SymbolId, SymbolKind};
use lasso::{Key, Rodeo, Spur};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Current packed cache schema version. Bumped from v6 (Phase B's JSON
/// wire) — every existing v6 JSON cache fails the version check on
/// load and triggers the documented silent-re-index path.
pub const CACHE_VERSION: u32 = 7;

/// 4-byte native-endian probe at file offset 0. A reader whose host
/// endianness disagrees with the writer's reads a different `u32`
/// value here and trips the silent-re-index branch — handling the
/// rare-but-real case of an Apple ARM Mac cache being mmap'd by an
/// Intel Linux container with the same checkout volume-mounted.
/// `0x01020304` was chosen so the on-disk bytes (e.g. `04 03 02 01`
/// on LE, `01 02 03 04` on BE) are self-describing in a hex-dump.
pub const ENDIAN_PROBE: u32 = 0x0102_0304;

/// Combined size of the header (probe + version), bytes.
pub const HEADER_SIZE: usize = 8;

/// Generator stamp written into the cache for diagnostic visibility.
const GENERATOR: &str = "code-graph-graph (rust, v7 packed rkyv)";

// ---------------------------------------------------------------------------
// Wire-form DTO (rkyv-archived)
// ---------------------------------------------------------------------------

/// On-disk shape. Keys in every map are `u32` ids resolved via
/// [`paths`](Self::paths) / [`names`](Self::names).
///
/// `Vec<PathBuf>` would be the natural type for `paths` but rkyv has
/// no built-in `Path`/`PathBuf` impl (path semantics are OS-specific).
/// We store `String` instead; encode/decode convert at the boundary
/// via [`Path::to_string_lossy`]. Non-UTF-8 paths on Windows / Unix
/// would round-trip lossily, but every path in this codebase is
/// dunce-canonicalized at index time (see `code_graph_core::paths`)
/// and the canonical form is UTF-8 on every supported platform.
#[derive(Archive, RkyvSerialize, RkyvDeserialize)]
pub(crate) struct PackedCacheV6 {
    pub version: u32,
    pub generator: String,
    pub last_sweep_at: u64,

    /// Path interner table. `paths[i]` is the path whose `PathId` is
    /// `i as u32 + 1` (since `PathId(0)` is the "no path" sentinel).
    /// Stored as `String` (see type-level doc-comment).
    pub paths: Vec<String>,

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
/// interner reference. `0` in `namespace` / `parent` means "absent"
/// (replaces v6 JSON's `Option<u32>` + `skip_serializing_if`; rkyv
/// archives `Option` cheaply but the zero-sentinel keeps the on-disk
/// size constant per-symbol).
#[derive(Archive, RkyvSerialize, RkyvDeserialize)]
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
    pub signature: String,
    /// NameId; `0` means absent.
    pub namespace: u32,
    /// NameId; `0` means absent.
    pub parent: u32,
    pub language: Language,
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize)]
pub(crate) struct PackedEdge {
    pub target: u32, // NameId — the OTHER endpoint's interned SymbolId
    pub kind: EdgeKind,
    pub file: u32, // PathId where the edge was declared
    pub line: u32,
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize)]
pub(crate) struct PackedFile {
    pub language: Language,
    pub symbol_ids: Vec<u32>, // NameId values
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize)]
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

    /// Same as [`intern`](Self::intern) but returns `0` for empty input
    /// (the absent-name sentinel for v7's `PackedSymbol.namespace` /
    /// `parent` u32 fields).
    fn intern_or_zero(&mut self, s: &str) -> u32 {
        if s.is_empty() {
            0
        } else {
            self.intern(s)
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
    /// Insertion-ordered Vec of paths-as-strings; index = `PathId - 1`.
    /// Stored as `String` to match the rkyv-archivable PackedCacheV6.paths
    /// field type (rkyv has no Path impl — see PackedCacheV6 doc-comment).
    order: Vec<String>,
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
            self.order.push(p.to_string_lossy().into_owned());
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

    fn into_vec(self) -> Vec<String> {
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
            namespace: names.intern_or_zero(&sym.namespace),
            parent: names.intern_or_zero(&sym.parent),
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
            namespace: resolver.name_or_empty(packed.namespace)?.to_string(),
            parent: resolver.name_or_empty(packed.parent)?.to_string(),
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
        let path = PathBuf::from(resolver.path(*path_id)?);
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
        let path = PathBuf::from(resolver.path(*path_id)?);
        let entries: Result<Vec<IncludeEntry>, DecodeError> = packed_entries
            .iter()
            .map(|pe| {
                Ok(IncludeEntry {
                    path: PathBuf::from(resolver.path(pe.path)?),
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
    paths: &'a [String],
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

    /// Resolve `PathId` to `&str`. `0` is invalid in this context —
    /// callers that allow the sentinel use [`path_to_string`] which
    /// returns `""`.
    fn path(&self, id: u32) -> Result<&str, DecodeError> {
        if id == 0 {
            return Err(DecodeError::PathOutOfRange(0));
        }
        self.paths
            .get(id as usize - 1)
            .map(String::as_str)
            .ok_or(DecodeError::PathOutOfRange(id))
    }

    /// Resolve `PathId` to a string for `Symbol.file` / `EdgeEntry.file`
    /// shape preservation. `0` resolves to `""`.
    fn path_to_string(&self, id: u32) -> String {
        if id == 0 {
            return String::new();
        }
        self.paths.get(id as usize - 1).cloned().unwrap_or_default()
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

    /// Resolve `NameId` to `&str`, with `0` meaning "no name" (returns
    /// `""`). Matches the encoder's `intern_or_zero` sentinel used for
    /// `PackedSymbol.namespace` / `parent`.
    fn name_or_empty(&self, id: u32) -> Result<&str, DecodeError> {
        if id == 0 {
            Ok("")
        } else {
            self.name(id)
        }
    }
}

// ---------------------------------------------------------------------------
// stale_paths slim DTO
// ---------------------------------------------------------------------------

// v7 stale_paths reads `paths` + `mtimes` directly off the archived
// view (`<PackedCacheV6 as Archive>::Archived`) in
// [`super::stale_paths`]. No dedicated DTO needed — the archived
// HashMap iterates as `(&Archived<u32>, &Archived<u64>)` pairs, and
// `Archived<u32>` converts back to `u32` cheaply. See PackedCache
// design Decision 6 for the rationale.
