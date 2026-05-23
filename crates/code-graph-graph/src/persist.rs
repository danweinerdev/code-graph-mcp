//! Versioned cache persistence for the in-memory [`Graph`] (current
//! schema: `CACHE_VERSION`).
//!
//! The on-disk cache lives at `<dir>/.code-graph-cache.json`. Two changes
//! versus the Go binary's v1 format (`internal/graph/persist.go`):
//!
//! 1. **`FileEntry { language, symbol_ids }`** instead of `[]string`. The
//!    `Language` recorded on `FileEntry` is persisted on disk so [`load`]
//!    does not have to re-derive it from the file extension.
//! 2. **Atomic save**. v1 wrote directly to the final path with
//!    `os.WriteFile`, leaving a partial-write window if the process died
//!    mid-flush. v2 writes to `<dir>/.code-graph-cache.json.tmp`, calls
//!    [`File::sync_all`] to flush data and metadata, then renames over the
//!    final path. The rename is atomic on POSIX and on Windows since Rust
//!    1.84 (which the workspace already requires).
//!
//! Version handling:
//! - current-version cache → loaded.
//! - v1 cache (Go-written), an older Rust cache, or any other version →
//!   silent re-index (`Ok(false)`). Version mismatch is **not** an error;
//!   it is the expected outcome the first time the Rust binary runs
//!   against a cache produced by the Go binary, and also whenever the
//!   schema is bumped (no transparent migration is attempted across a
//!   schema break).
//! - JSON parse errors and IO errors → loud (`Err(PersistError::*)`).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use code_graph_core::{paths, Symbol, SymbolId};
use serde::{Deserialize, Serialize};

use crate::graph::{EdgeEntry, FileEntry, Graph, IncludeEntry, Node};

const CACHE_FILE_NAME: &str = ".code-graph-cache.json";
// v3: include entries now carry the source line of the `#include`
// directive (was a bare path list) so the dependency query can report
// where each include was declared. The shape change is not
// backward-compatible; a v2 cache fails the version check below and the
// caller silently re-indexes (no transparent migration is attempted).
//
// v4: Rust symbol record VALUES changed shape. Two cached fields are
// affected:
// (1) `Symbol.namespace` — Rust namespaces are now crate-qualified
// (`<crate>::<module-path>` instead of empty/inline-only) after the
// `LanguagePlugin::post_index` hook rewrites them at index time;
// (2) `Symbol.kind` / `Symbol.parent` for default and abstract trait
// methods — these now classify as `Method` with `parent = trait_name`
// rather than the prior `Function` classification.
// The persisted JSON SHAPE is identical (same fields, same types), but
// the *values* on a v3 cache no longer reflect what a fresh re-parse
// produces, so honoring a v3 cache would yield stale symbol data. The
// version-check branch in `Graph::load` returns `Ok(false)` on mismatch
// → silent re-index, no `force=true` required, no transparent migration
// attempted (mirrors the v2→v3 precedent).
//
// v5: cache adds `last_sweep_at` (nanoseconds since UNIX_EPOCH) tracking
// when the out-of-scope hygiene sweep last ran. The sweep runs on a
// time-based cadence (see `SWEEP_INTERVAL_NANOS`) inside
// `analyze_codebase` to drop ghost cache entries for files that were
// deleted in subtrees the current invocation doesn't touch. The field
// is `u64` with `#[serde(default)]` so the absence on a v4 cache
// would deserialize as `0` — but the version bump means v4 caches
// fail the version check before deserialization can recover the
// missing field anyway. Same invalidation contract as the v3→v4 bump:
// version mismatch → `Ok(false)` from `Graph::load` → caller silently
// re-indexes.
const CACHE_VERSION: u32 = 5;

/// Out-of-scope sweep cadence: 24 hours in nanoseconds. After a scoped
/// `analyze_codebase` finishes its in-scope work, if at least this
/// many nanoseconds have elapsed since the last sweep, the handler
/// runs `Graph::sweep_missing_out_of_scope` to stat every cached file
/// OUTSIDE the invocation scope and drop the ones missing on disk.
/// Keeps the cache from accumulating ghost entries without paying
/// full-revalidation cost on every invocation.
pub const SWEEP_INTERVAL_NANOS: u64 = 24 * 60 * 60 * 1_000_000_000;
const GENERATOR: &str = "code-graph-graph (rust)";

/// Errors returned by [`Graph::save`], [`Graph::load`], and [`stale_paths`].
///
/// Version-mismatch and missing-file are **not** errors — they surface as
/// `Ok(false)` on `load` so the caller can silently re-index.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// On-disk DTO. Lives in this module (not in `graph.rs`) because the
/// `nodes` shape diverges from `Graph::nodes`: the cache stores bare
/// [`Symbol`]s rather than [`Node`] wrappers, matching the Go v1 layout
/// and avoiding a redundant `{"symbol": {...}}` wrapper in JSON.
#[derive(Serialize, Deserialize)]
struct GraphCache {
    version: u32,
    generator: String,
    nodes: HashMap<SymbolId, Symbol>,
    adj: HashMap<SymbolId, Vec<EdgeEntry>>,
    radj: HashMap<SymbolId, Vec<EdgeEntry>>,
    files: HashMap<PathBuf, FileEntry>,
    includes: HashMap<PathBuf, Vec<IncludeEntry>>,
    /// Nanoseconds since UNIX_EPOCH per indexed file. Stored as `u64`
    /// (Go uses `int64`); truncation is fine because negative timestamps
    /// are not real on any filesystem we support. Files whose mtime cannot
    /// be read are recorded as `0` so they look stale on the next check.
    mtimes: HashMap<PathBuf, u64>,
    /// Nanoseconds since UNIX_EPOCH when the project-wide
    /// out-of-scope hygiene sweep last ran. `0` means "never swept"
    /// (or pre-v5 cache that was migrated forward, though we don't
    /// migrate so this only ever happens on a freshly-built cache
    /// before the first sweep). The handler compares this against the
    /// current clock + [`SWEEP_INTERVAL_NANOS`] to decide whether the
    /// current invocation should run a sweep before saving.
    #[serde(default)]
    last_sweep_at: u64,
}

/// Borrowing variant of [`GraphCache`] used only on the save path.
///
/// Field declaration order and types mirror `GraphCache` so the
/// serialized JSON is byte-identical to the prior owned-DTO output, but
/// the live [`Graph`] maps are borrowed rather than cloned. On a
/// multi-million-symbol graph the clone was an O(graph) allocation that
/// briefly doubled resident memory; borrowing avoids it entirely.
///
/// `nodes` is bridged by [`serialize_nodes_as_symbols`] so the JSON
/// shape stays flat (no `{"symbol": {...}}` wrapper), matching
/// `GraphCache.nodes` (the owned DTO consumed by [`Graph::load`])
/// exactly. `mtimes` stays owned because it is freshly computed from
/// the filesystem at save time — it does not live in [`Graph`].
#[derive(Serialize)]
struct GraphCacheRef<'a> {
    version: u32,
    generator: &'static str,
    #[serde(serialize_with = "serialize_nodes_as_symbols")]
    nodes: &'a HashMap<SymbolId, Node>,
    adj: &'a HashMap<SymbolId, Vec<EdgeEntry>>,
    radj: &'a HashMap<SymbolId, Vec<EdgeEntry>>,
    files: &'a HashMap<PathBuf, FileEntry>,
    includes: &'a HashMap<PathBuf, Vec<IncludeEntry>>,
    mtimes: HashMap<PathBuf, u64>,
    last_sweep_at: u64,
}

/// Slim DTO consumed only by [`stale_paths`]. Materializes only
/// `mtimes` (the field the stat-and-compare loop reads); every other
/// JSON field — including `version` — is parsed-and-skipped by
/// `serde_json` rather than allocated into a Rust value. Drops the
/// `stale_paths` heap footprint from "size of the full deserialized
/// graph" to "size of the `mtimes` map" — a ~3-4 GB→tens-of-MB win on
/// multi-million-symbol caches. Serde's default behavior is to ignore
/// JSON fields absent from the target struct (no `deny_unknown_fields`
/// on this type or its peers), which is what makes the skip work.
///
/// Version-check intentionally omitted here: the only caller in
/// `analyze_codebase` runs `Graph::load` first and only invokes
/// `stale_paths` when load succeeded, so the version match is already
/// established. Keeping the DTO minimal saves both the field and the
/// branch.
#[derive(Deserialize)]
struct StalePathsCache {
    mtimes: HashMap<PathBuf, u64>,
}

/// Serialize `&HashMap<SymbolId, Node>` as if it were
/// `HashMap<SymbolId, Symbol>` — emit `node.symbol` directly so the JSON
/// shape matches `GraphCache.nodes` (the owned DTO consumed by
/// [`Graph::load`]) exactly, with no `{"symbol": {...}}` wrapper.
fn serialize_nodes_as_symbols<S>(
    nodes: &HashMap<SymbolId, Node>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeMap;
    let mut map = serializer.serialize_map(Some(nodes.len()))?;
    for (id, node) in nodes {
        map.serialize_entry(id, &node.symbol)?;
    }
    map.end()
}

/// Resolve the cache file path for the given directory. Exposed for tests
/// and for callers that need to delete the cache (e.g. `force=true` on
/// `analyze_codebase`).
pub fn cache_path(dir: &Path) -> PathBuf {
    dir.join(CACHE_FILE_NAME)
}

/// Read a file's mtime as nanoseconds since UNIX_EPOCH. Returns `None` if
/// the file does not exist, is unreadable, or has a pre-epoch mtime.
fn mtime_nanos(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
}

/// Simplify the file-path portion of a [`SymbolId`].
///
/// A `SymbolId` is `<file>:<name>` for free symbols and `<file>:<parent>::<name>`
/// for methods (see `code_graph_core::symbol_id`). This helper splits on the
/// **rightmost `:` that is not part of a `::` token** — the exact same rule
/// `code_graph_core::id_to_file` uses — applies [`paths::simplify`] to the
/// file portion, and rejoins with the original name portion.
///
/// On an id with no qualifying separator (no `:` at all, or only `::` tokens),
/// the id is returned unchanged via the fallback branch.
///
/// A leading `:` (e.g. `":foo"`) is a special-but-degenerate case: the index-0
/// colon IS found as a qualifying separator (no left neighbor, no right `:`),
/// so the split-rejoin branch runs with an empty file portion. `paths::simplify`
/// on an empty path round-trips to empty, and the rejoin produces the original
/// id — identity via arithmetic, not via the fallback. Matches `id_to_file`'s
/// behavior (which returns `""` as the file portion for `":foo"`).
///
/// **Contract dependency:** this helper mirrors `code_graph_core::id_to_file`'s
/// splitting rule. If `symbol_id`/`id_to_file`'s format ever changes (e.g. a
/// new separator), this helper must be updated in lockstep — the two split
/// the same way for the same reason.
fn simplify_symbol_id(id: &str) -> String {
    let bytes = id.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        if bytes[i] == b':' {
            let prev_is_colon = i > 0 && bytes[i - 1] == b':';
            let next_is_colon = i + 1 < bytes.len() && bytes[i + 1] == b':';
            if !prev_is_colon && !next_is_colon {
                // Qualifying separator at byte index `i`. Slice `[..i]` is
                // the file portion, `[i..]` is the leading `:` plus the
                // name portion. Splitting at `i` (an ASCII colon) is safe
                // for UTF-8 — `b':'` is single-byte.
                let file_part = &id[..i];
                let name_part = &id[i..];
                let simplified_file = paths::simplify(Path::new(file_part));
                let mut out =
                    String::with_capacity(simplified_file.as_os_str().len() + name_part.len());
                out.push_str(&simplified_file.to_string_lossy());
                out.push_str(name_part);
                return out;
            }
        }
    }
    // No qualifying separator → leave the id untouched. Mirrors
    // `id_to_file`'s "returns empty" signal for prefixless/malformed ids;
    // here we have no `<file>` to rewrite, so the rewrite is the identity.
    id.to_string()
}

/// Walk every path-bearing field of `cache` and simplify in place.
///
/// Runs unconditionally on every successfully-loaded cache (any
/// schema version). Its job is the path-normalization rewrite: older
/// caches' path strings may still carry the Windows verbatim-extended
/// prefix (`\\?\`). The `PathNormalization` work switched the indexer
/// to write short-form paths via `paths::canonicalize`; a cache
/// written before that needs an in-place rewrite before it
/// materializes into a [`Graph`], or the resulting graph is silently
/// inconsistent (e.g. `files` keys stripped while `nodes` SymbolId
/// keys still prefixed → every file lookup returns empty with no
/// error). On an already-clean cache the rewrite is a no-op.
///
/// The 12 path-bearing locations rewritten (see Decision 5 of
/// `Designs/PathNormalization/README.md`):
/// 1. `files` map keys
/// 2. every `FileEntry.symbol_ids` entry (file-portion of the SymbolId)
/// 3. `nodes` map keys (file-portion of the SymbolId)
/// 4. every `Symbol.file` string inside a node value
/// 5. `adj` map keys
/// 6. every `EdgeEntry.target` inside `adj` (file-portion of the SymbolId)
/// 7. every `EdgeEntry.file` inside `adj`
///    8/9/10. the same three fields inside `radj`
/// 11. `includes` map keys AND every inner `IncludeEntry.path` (the
///     `line` field carries no path and is left untouched)
/// 12. `mtimes` map keys
///
/// **Idempotency.** [`paths::simplify`] is identity on non-extended paths,
/// so a second call on the same cache produces byte-identical output. The
/// rebuild-by-drain pattern is also deterministic in the sense that field
/// ordering inside `HashMap`s is irrelevant for equality.
///
/// Returns `true` iff at least one path-bearing key in `cache` starts
/// with the Windows extended-path prefix `\\?\`. Drives the
/// short-circuit in [`simplify_cache`]: on a cache produced by any
/// post-`PathNormalization` build, this returns `false` and load
/// skips ~5 full HashMap rebuilds.
///
/// Probes one representative entry per map (file keys, node SymbolIds,
/// adj/radj keys, include keys, mtime keys). One contaminated entry
/// anywhere is sufficient evidence that the writer was pre-PathNorm —
/// the writer canonicalizes every field together, so contamination is
/// all-or-nothing per cache.
fn cache_needs_simplify(cache: &GraphCache) -> bool {
    const PREFIX: &str = r"\\?\";
    if cache
        .files
        .keys()
        .next()
        .is_some_and(|p| p.to_string_lossy().starts_with(PREFIX))
    {
        return true;
    }
    if cache
        .nodes
        .keys()
        .next()
        .is_some_and(|id| id.starts_with(PREFIX))
    {
        return true;
    }
    if cache
        .adj
        .keys()
        .next()
        .is_some_and(|id| id.starts_with(PREFIX))
    {
        return true;
    }
    if cache
        .radj
        .keys()
        .next()
        .is_some_and(|id| id.starts_with(PREFIX))
    {
        return true;
    }
    if cache
        .includes
        .keys()
        .next()
        .is_some_and(|p| p.to_string_lossy().starts_with(PREFIX))
    {
        return true;
    }
    if cache
        .mtimes
        .keys()
        .next()
        .is_some_and(|p| p.to_string_lossy().starts_with(PREFIX))
    {
        return true;
    }
    false
}

/// Helper is private; the only caller is `Graph::load`.
fn simplify_cache(cache: &mut GraphCache) {
    // Cheap path-cleanliness sniff. The `\\?\` Windows extended-path
    // prefix is the only thing `paths::simplify` ever rewrites; on a
    // cache where no path carries that prefix, every drain-and-rebuild
    // below is a no-op (identity-by-arithmetic) that still pays the
    // cost of 5 full HashMap rebuilds + per-key allocations. On a
    // 770k-symbol cache that adds seconds to load. We probe one
    // representative key per path-bearing map and bail before any
    // mutation if none of the probed entries carry the prefix.
    //
    // Correctness rests on the writer's invariant: every cached path
    // flows through `paths::canonicalize` at index time, which strips
    // `\\?\` consistently across all fields. So if a contaminated
    // entry exists in ANY field, the writer that produced this cache
    // also wrote contaminated entries in EVERY field — we cannot have
    // clean `files` keys alongside dirty `nodes` SymbolIds. Sniffing
    // every map keeps the predicate defensive even if a future writer
    // partially regresses; the cost is `O(maps)` not `O(entries)`.
    if !cache_needs_simplify(cache) {
        return;
    }

    // (1, 2) files map + each FileEntry's symbol_ids.
    let old_files = std::mem::take(&mut cache.files);
    cache.files.reserve(old_files.len());
    for (path, mut entry) in old_files {
        let new_path = paths::simplify(&path);
        for sid in &mut entry.symbol_ids {
            *sid = simplify_symbol_id(sid);
        }
        cache.files.insert(new_path, entry);
    }

    // (3, 4) nodes map keys + each Symbol.file value.
    let old_nodes = std::mem::take(&mut cache.nodes);
    cache.nodes.reserve(old_nodes.len());
    for (id, mut symbol) in old_nodes {
        let new_id = simplify_symbol_id(&id);
        symbol.file = paths::simplify(Path::new(&symbol.file))
            .to_string_lossy()
            .into_owned();
        cache.nodes.insert(new_id, symbol);
    }

    // (5, 6, 7) adj map keys + each EdgeEntry's target SymbolId + file PathBuf.
    simplify_edge_map(&mut cache.adj);
    // (8, 9, 10) same three for radj.
    simplify_edge_map(&mut cache.radj);

    // (11) includes map keys AND each inner IncludeEntry.path (line kept).
    let old_includes = std::mem::take(&mut cache.includes);
    cache.includes.reserve(old_includes.len());
    for (path, mut deps) in old_includes {
        let new_path = paths::simplify(&path);
        for dep in &mut deps {
            dep.path = paths::simplify(&dep.path);
        }
        cache.includes.insert(new_path, deps);
    }

    // (12) mtimes map keys.
    let old_mtimes = std::mem::take(&mut cache.mtimes);
    cache.mtimes.reserve(old_mtimes.len());
    for (path, nanos) in old_mtimes {
        cache.mtimes.insert(paths::simplify(&path), nanos);
    }
}

/// Drain-and-rebuild an adjacency map: simplify each SymbolId key, and for
/// each `EdgeEntry` in the vec simplify both `target` (SymbolId) and `file`
/// (PathBuf). Shared between `adj` and `radj`.
fn simplify_edge_map(map: &mut HashMap<SymbolId, Vec<EdgeEntry>>) {
    let old = std::mem::take(map);
    map.reserve(old.len());
    for (id, mut entries) in old {
        let new_id = simplify_symbol_id(&id);
        for entry in &mut entries {
            entry.target = simplify_symbol_id(&entry.target);
            entry.file = paths::simplify(&entry.file);
        }
        map.insert(new_id, entries);
    }
}

impl Graph {
    /// Atomically write the graph to `<dir>/.code-graph-cache.json`.
    ///
    /// Strategy: serialize, write to a sibling `.tmp` file, [`File::sync_all`]
    /// to flush data and metadata, then [`fs::rename`] to swap the tmp
    /// file over the final path. The rename is atomic on POSIX and on
    /// Windows since Rust 1.84.
    ///
    /// Failure modes:
    /// - Tmp file create / write failure → `Err(PersistError::Io)`; the
    ///   final cache (if any) is unchanged.
    /// - Serialization failure → `Err(PersistError::Json)`; nothing is
    ///   written.
    /// - Rename failure (rare) → `Err(PersistError::Io)`; the tmp file may
    ///   be left behind but the final cache is untouched.
    ///
    /// The explicit braces around the `File` keep the file handle in scope
    /// for the [`File::sync_all`] call. Dropping the `File` only closes it;
    /// it does **not** fsync. Without the explicit sync, a crash before
    /// the OS flush window would still produce a partial write.
    pub fn save(&self, dir: &Path) -> Result<(), PersistError> {
        let final_path = cache_path(dir);
        let tmp_path = dir.join(format!("{CACHE_FILE_NAME}.tmp"));

        // Read mtimes for every indexed file. Files that no longer exist
        // record `0` so a future `stale_paths` call flags them.
        let mut mtimes = HashMap::with_capacity(self.files.len());
        for path in self.files.keys() {
            mtimes.insert(path.clone(), mtime_nanos(path).unwrap_or(0));
        }

        // Borrowing DTO + streaming writer. The prior shape cloned every
        // internal map into an owned `GraphCache` and then `to_vec`-ed it
        // into a single `Vec<u8>` before any byte hit disk — on a
        // multi-million-symbol graph that briefly held the entire graph
        // twice (live + cloned) plus the serialized JSON as a third
        // copy, pushing save-time peak RSS to roughly 2× the in-memory
        // footprint. The new path borrows the live maps and streams
        // straight into a buffered `File`, keeping peak RSS close to
        // the graph's resident size. `Node` is bridged to `Symbol` via
        // `serialize_nodes_as_symbols` so the on-disk JSON shape stays
        // byte-identical to what `GraphCache` produced.
        let cache = GraphCacheRef {
            version: CACHE_VERSION,
            generator: GENERATOR,
            nodes: &self.nodes,
            adj: &self.adj,
            radj: &self.radj,
            files: &self.files,
            includes: &self.includes,
            mtimes,
            last_sweep_at: self.last_sweep_at,
        };

        // Write-tmp → flush BufWriter → sync_all → rename. The braces
        // matter: `sync_all` must run while the `File` is still open.
        //
        // Trade-off versus the prior `to_vec`-first design: a
        // serialization failure mid-stream can leave a partial tmp file
        // behind. The final cache is still untouched (the rename only
        // fires on the success path) and the next save overwrites the
        // tmp at the same path — `save_does_not_disturb_unrelated_tmp_file`
        // pins that a stale tmp from a crashed run does not block a
        // subsequent save.
        {
            let f = File::create(&tmp_path)?;
            let mut writer = io::BufWriter::new(f);
            serde_json::to_writer(&mut writer, &cache)?;
            writer.flush()?;
            let f = writer
                .into_inner()
                .map_err(|e| io::Error::other(e.into_error()))?;
            f.sync_all()?;
        }
        fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    /// Load the cache from `<dir>/.code-graph-cache.json`.
    ///
    /// Returns:
    /// - `Ok(true)` — cache loaded; graph state replaced.
    /// - `Ok(false)` — cache absent, or the on-disk JSON parses cleanly but
    ///   has a `version` field that doesn't equal [`CACHE_VERSION`]. The
    ///   graph is unchanged; the caller should re-index.
    /// - `Err(PersistError::Io)` — read failure other than not-found.
    /// - `Err(PersistError::Json)` — corrupt JSON, **including** any
    ///   real-world Go-produced v1 cache. Go's `EdgeEntry` lacks JSON tags
    ///   (so it serializes as `"Target"`/`"Kind"`/...) and Go's `Symbol`
    ///   has no `language` field; both shape mismatches surface as
    ///   `PersistError::Json`. The handler in `analyze_codebase` (Phase
    ///   3.4) should treat any `Err` here the same as `Ok(false)` — drop
    ///   the cache, re-index. The structurally-valid `version !=
    ///   CACHE_VERSION` path is exercised by Rust→Rust schema bumps: a
    ///   cache written by an older Rust version parses cleanly but trips
    ///   the version check and falls through to a full re-index, with no
    ///   transparent migration.
    pub fn load(&mut self, dir: &Path) -> Result<bool, PersistError> {
        let path = cache_path(dir);
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(PersistError::Io(e)),
        };
        // Stream the JSON parse straight off disk rather than first
        // buffering the entire on-disk cache as a `Vec<u8>` via
        // `fs::read`. Mirrors the save-path BufWriter switch in
        // [`Graph::save`]: on multi-GB caches the prior `from_slice`
        // path held a 2.9 GB+ source-bytes Vec alongside the
        // half-built `GraphCache` HashMaps. `serde_json::from_reader`
        // is known to be slower than `from_slice` because it dispatches
        // through `Read` per byte; `BufReader::with_capacity(256K)`
        // amortizes that to ~negligible while the heap footprint of the
        // reader itself stays trivial.
        let reader = io::BufReader::with_capacity(256 * 1024, file);
        let mut cache: GraphCache = serde_json::from_reader(reader)?;
        if cache.version != CACHE_VERSION {
            // v1 (Go-written), v2/v3 (older Rust schemas), or any future
            // version: silent re-index. Graph state is left untouched on
            // this branch. No transparent migration is attempted across a
            // schema break — see the `CACHE_VERSION` constant doc-comment
            // for the per-bump rationale.
            return Ok(false);
        }

        // Migrate path-bearing fields in place. On already-clean caches
        // this is a no-op (per `paths::simplify`'s identity behavior on
        // non-extended paths). Legacy caches written before path
        // canonicalization shipped — i.e. with Windows verbatim-extended
        // (`\\?\`) prefixes throughout — get rewritten to short-form
        // here, before the cache is consumed into the in-memory graph.
        simplify_cache(&mut cache);

        self.nodes = cache
            .nodes
            .into_iter()
            .map(|(id, symbol)| (id, Node { symbol }))
            .collect();
        self.adj = cache.adj;
        self.radj = cache.radj;
        self.files = cache.files;
        self.includes = cache.includes;
        // Restore the sweep timestamp so the cadence survives process
        // restarts. A pre-v5 cache (which fails the version check
        // above) never reaches this line; a v5 cache always has the
        // field, defaulting to `0` for a freshly-built cache that has
        // not yet had a sweep run.
        self.last_sweep_at = cache.last_sweep_at;
        Ok(true)
    }
}

/// Returns indexed files whose on-disk mtime differs from the cached
/// mtime. Files that no longer exist (or whose mtime cannot be read) are
/// included so the indexer treats them as stale and re-walks them.
///
/// Reads `<dir>/.code-graph-cache.json` and inspects only the `mtimes`
/// field; other fields (nodes, edges, etc.) are still deserialized but
/// ignored, which is acceptable for the caller's hot path because
/// `stale_paths` runs at most once per `analyze_codebase` call.
///
/// **Missing cache:** returns `Ok(vec![])`. This matches `Graph::load`'s
/// `Ok(false)` ergonomics — callers can speculatively call `stale_paths`
/// before `load` without a special-case for first-run.
pub fn stale_paths(dir: &Path) -> Result<Vec<PathBuf>, PersistError> {
    let path = cache_path(dir);
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(PersistError::Io(e)),
    };
    // Streaming reader + slim DTO. The prior `fs::read` →
    // `from_slice::<GraphCache>` path allocated the full graph
    // (~3-4 GB on a multi-million-symbol cache) only to read one field
    // off it. `StalePathsCache` deserializes `version` + `mtimes`
    // alone; the rest of the JSON is still parsed by `serde_json`'s
    // stream — there is no JSON-level skip — but the un-mentioned
    // fields are dropped on the floor rather than materialized into
    // HashMaps. Shape-stable across every `CACHE_VERSION` bump so
    // far (mtimes has carried the same `HashMap<PathBuf, u64>` shape
    // since v1).
    let reader = io::BufReader::with_capacity(256 * 1024, file);
    let cache: StalePathsCache = serde_json::from_reader(reader)?;

    // Intentionally does NOT call `simplify_cache` (see PathNormalization
    // design Decision 5). `stale_paths` only consumes `cache.mtimes` for the
    // `mtime_nanos` stat-and-compare loop below; rewriting the keys would
    // both waste work (9 of 10 cache fields are dropped here) and risk
    // reformatting paths that `mtime_nanos` needs in their on-disk form
    // (paths > 260 chars on Windows may require the extended-path prefix).
    //
    // Windows legacy-cache migration story: a cache written before
    // `paths::canonicalize` shipped (with `\\?\`-prefixed `mtimes` keys)
    // flows through `stale_paths` unchanged. The `mtime_nanos` call either
    // succeeds (because Windows accepts both forms for stat) and the cache
    // fast-path proceeds normally — then `Graph::load` runs and applies
    // `simplify_cache` so the in-memory graph is consistent. Or
    // `mtime_nanos` returns `None` for every file, all are reported stale,
    // and `analyze_codebase` falls through to a full re-index (which uses
    // `paths::canonicalize` to write clean keys going forward). Either
    // outcome is correct; the forced re-index is graceful degradation, not
    // a regression.
    let mut stale = Vec::new();
    for (path, cached_nanos) in &cache.mtimes {
        match mtime_nanos(path) {
            None => stale.push(path.clone()),
            Some(c) if c != *cached_nanos => stale.push(path.clone()),
            _ => {}
        }
    }
    Ok(stale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::{call_edge, include_edge, inherit_edge, make_fg, sym};
    use code_graph_core::{EdgeKind, Language, SymbolKind};
    use pretty_assertions::assert_eq;
    use std::fs::OpenOptions;
    use tempfile::TempDir;

    /// Build a small graph with a mix of edge kinds and two files. Used by
    /// the round-trip and atomic-save tests. The files referenced are not
    /// real on disk; that is fine — `save` records their mtimes as `0` and
    /// `stale_paths` would flag them, but neither test inspects mtimes.
    fn build_sample_graph() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
                sym("Base", SymbolKind::Class, "/a.cpp"),
                sym("Derived", SymbolKind::Class, "/a.cpp"),
            ],
            vec![
                call_edge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp", 7),
                inherit_edge("Derived", "Base", "/a.cpp"),
                include_edge("/a.cpp", "/utils.h", "/a.cpp"),
            ],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("baz", SymbolKind::Function, "/b.cpp")],
            vec![call_edge("/b.cpp:baz", "/a.cpp:foo", "/b.cpp", 3)],
        ));
        g
    }

    #[test]
    fn save_load_round_trip() {
        let dir = TempDir::new().unwrap();
        let original = build_sample_graph();

        original.save(dir.path()).unwrap();

        let mut loaded = Graph::new();
        let ok = loaded.load(dir.path()).unwrap();
        assert!(ok, "load should report success on a freshly written cache");

        // Storage maps must match exactly.
        assert_eq!(loaded.nodes, original.nodes);
        assert_eq!(loaded.adj, original.adj);
        assert_eq!(loaded.radj, original.radj);
        assert_eq!(loaded.files, original.files);
        assert_eq!(loaded.includes, original.includes);
        // Stats sanity-check: 5 nodes, 3 adj edges + 1 include = 4, 2 files.
        let stats = loaded.stats();
        assert_eq!(stats.nodes, 5);
        assert_eq!(stats.edges, 4);
        assert_eq!(stats.files, 2);
    }

    /// `last_sweep_at` (v5 schema field) must survive save/load.
    ///
    /// `save_load_round_trip` above uses the default sample graph
    /// (last_sweep_at = 0) and therefore couldn't tell apart "field
    /// preserved" from "field dropped on the wire and re-defaulted on
    /// load". This test plants a distinctive non-zero value, round-trips
    /// it, and asserts equality. A regression that dropped the field
    /// from `GraphCacheRef` serialization or from the `Graph::load`
    /// restore step would slip past `save_load_round_trip` but break
    /// here.
    #[test]
    fn save_load_round_trip_preserves_last_sweep_at() {
        let dir = TempDir::new().unwrap();
        let mut original = build_sample_graph();
        // Pick a distinctive value that's neither 0 (the default) nor
        // a round number that could be coincidentally produced by some
        // arithmetic. ~year 2026 timestamp in nanoseconds, +12345 to
        // make it unmistakable.
        const SENTINEL_NANOS: u64 = 1_700_000_000_000_000_000 + 12345;
        original.set_last_sweep_at(SENTINEL_NANOS);

        original.save(dir.path()).unwrap();

        let mut loaded = Graph::new();
        let ok = loaded.load(dir.path()).unwrap();
        assert!(ok, "freshly-written cache must load");
        assert_eq!(
            loaded.last_sweep_at(),
            SENTINEL_NANOS,
            "last_sweep_at must round-trip through save/load — \
             a regression dropping the field would default it to 0"
        );
    }

    /// `last_sweep_at` defaults to 0 on a fresh graph and survives the
    /// trivial round-trip without spontaneously changing. Pairs with
    /// the sentinel test above: that proves non-zero survives; this
    /// proves zero stays zero (no accidental
    /// "default-when-loaded-then-stamped-with-current-time" drift).
    #[test]
    fn save_load_round_trip_preserves_zero_last_sweep_at() {
        let dir = TempDir::new().unwrap();
        let original = build_sample_graph();
        assert_eq!(original.last_sweep_at(), 0, "fresh graph defaults to 0");

        original.save(dir.path()).unwrap();
        let mut loaded = Graph::new();
        loaded.load(dir.path()).unwrap();
        assert_eq!(
            loaded.last_sweep_at(),
            0,
            "zero last_sweep_at must remain zero across round-trip"
        );
    }

    #[test]
    fn load_missing_file_returns_false() {
        let dir = TempDir::new().unwrap();
        let mut g = Graph::new();
        // Pre-populate so we can prove load doesn't touch state on the
        // false path.
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
            vec![sym("x", SymbolKind::Function, "/x.cpp")],
            vec![],
        ));
        let before_nodes = g.nodes.clone();

        let ok = g.load(dir.path()).unwrap();
        assert!(!ok, "missing cache file → Ok(false)");
        assert_eq!(g.nodes, before_nodes, "graph must be untouched");
    }

    #[test]
    fn load_version_mismatch_returns_false() {
        // Tests the version-check branch only: a cache with `version: 1`
        // but otherwise structurally-valid (lowercase keys, etc.) parses
        // cleanly and trips the version-mismatch Ok(false) branch. Real
        // Go-produced v1 caches fail with PersistError::Json (different
        // schema). See load() doc-comment.
        // Hand-craft a minimal v1 (Go-shape) cache file. Only the version
        // field needs to be present and wrong; the other fields are
        // optional in serde_json's loose object parsing… except they
        // aren't, because GraphCache requires all of them. So include
        // them with valid empty values: the version check fires before
        // anything else matters.
        let dir = TempDir::new().unwrap();
        let v1_json = serde_json::json!({
            "version": 1,
            "generator": "go",
            "nodes": {},
            "adj": {},
            "radj": {},
            "files": {},
            "includes": {},
            "mtimes": {},
        });
        fs::write(cache_path(dir.path()), v1_json.to_string()).unwrap();

        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/y.cpp",
            Language::Cpp,
            vec![sym("y", SymbolKind::Function, "/y.cpp")],
            vec![],
        ));
        let before_nodes = g.nodes.clone();

        let ok = g.load(dir.path()).unwrap();
        assert!(!ok, "version mismatch → silent re-index (Ok(false))");
        assert_eq!(
            g.nodes, before_nodes,
            "graph must be unchanged on the false path"
        );
    }

    /// Pins the v3→v4 transition behavior.
    ///
    /// A structurally-valid cache whose `version` is the prior schema
    /// (`3`) must trip `Graph::load`'s version-check branch and surface as
    /// `Ok(false)` — silent re-index, no `force=true`, no transparent
    /// migration attempted, no panic. The graph's prior state must be
    /// untouched on the false path. This mirrors the existing v1/v2
    /// precedent (`load_version_mismatch_returns_false`); we add a
    /// dedicated v3 test here so a future regression that reverts the
    /// `CACHE_VERSION` bump (the symbol-shape change that necessitated
    /// the bump being silently honored) fires immediately on the v3
    /// corpus instead of waiting for an integration suite to notice
    /// stale data.
    ///
    /// Also asserts that a freshly-written v4 cache round-trips correctly,
    /// so the new schema number is observed end-to-end (build → save →
    /// load → equality).
    #[test]
    fn stale_v3_cache_returns_ok_false_silent_reindex() {
        // Hand-craft a v3-shape cache. All required `GraphCache` fields
        // are present with valid empty values; the version check fires
        // before any of the other fields are inspected, so we don't have
        // to mirror the actual v3 payload shape beyond the field set.
        let dir = TempDir::new().unwrap();
        let v3_json = serde_json::json!({
            "version": 3,
            "generator": "code-graph-graph (rust) pre-v4-bump",
            "nodes": {},
            "adj": {},
            "radj": {},
            "files": {},
            "includes": {},
            "mtimes": {},
        });
        fs::write(cache_path(dir.path()), v3_json.to_string()).unwrap();

        // Pre-populate the in-memory graph so we can prove `load` does
        // NOT touch state on the false path — same pattern as the
        // existing v1 test.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/keep.rs",
            Language::Rust,
            vec![sym("keep_me", SymbolKind::Function, "/keep.rs")],
            vec![],
        ));
        let before_nodes = g.nodes.clone();

        let ok = g.load(dir.path()).unwrap();
        assert!(
            !ok,
            "v3 cache must trip the version-check branch and return Ok(false) \
             so the caller silently re-indexes — no force=true required, no \
             transparent migration attempted",
        );
        assert_eq!(
            g.nodes, before_nodes,
            "graph state must be untouched when load returns Ok(false)",
        );

        // v4 fresh-write round-trip. Build a small graph, save to a
        // separate directory (so the v3 file doesn't shadow), then load
        // into a fresh `Graph` and prove the maps survive byte-identically.
        // This pins that CACHE_VERSION = 4 is observed by both `save`
        // (it writes 4 into the version field) and `load` (it accepts 4
        // as current).
        let dir_v4 = TempDir::new().unwrap();
        let original = build_sample_graph();
        original.save(dir_v4.path()).unwrap();

        // On-disk version must equal the current `CACHE_VERSION` — pin
        // the literal so a future accidental revert of any bump fires
        // this assert before any round-trip equality check could mask it.
        let bytes = fs::read(cache_path(dir_v4.path())).unwrap();
        let cache_on_disk: GraphCache = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            cache_on_disk.version, CACHE_VERSION,
            "freshly-written cache must carry version={CACHE_VERSION} (the current schema)",
        );

        let mut loaded = Graph::new();
        let ok = loaded.load(dir_v4.path()).unwrap();
        assert!(
            ok,
            "freshly-written current-version cache must load successfully (Ok(true))",
        );
        // `mtimes` is intentionally absent from this equality block: it
        // lives in `GraphCache`, not `Graph`, and `load` never assigns it
        // to `self` — there is nothing on the `Graph` side to compare
        // against.
        assert_eq!(loaded.nodes, original.nodes);
        assert_eq!(loaded.adj, original.adj);
        assert_eq!(loaded.radj, original.radj);
        assert_eq!(loaded.files, original.files);
        assert_eq!(loaded.includes, original.includes);
    }

    #[test]
    fn stale_paths_missing_cache_returns_empty_vec() {
        // Speculative call before any cache exists must not error — matches
        // load()'s Ok(false) ergonomics for first-run.
        let dir = TempDir::new().unwrap();
        let result = stale_paths(dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn load_invalid_json_returns_err() {
        let dir = TempDir::new().unwrap();
        fs::write(cache_path(dir.path()), b"this is not json {[").unwrap();

        let mut g = Graph::new();
        let err = g.load(dir.path()).unwrap_err();
        assert!(
            matches!(err, PersistError::Json(_)),
            "garbage JSON must surface as PersistError::Json, got {err:?}"
        );
    }

    #[test]
    fn save_overwrites_existing_cache_atomically() {
        // Stronger version of the atomic-save test: write a corrupt file
        // at the final path BEFORE `save`. The successful save must
        // replace it atomically with the new content. After the call
        // there must be no leftover `.tmp` file (the rename consumed it).
        let dir = TempDir::new().unwrap();
        let final_path = cache_path(dir.path());
        let tmp_path = dir.path().join(format!("{CACHE_FILE_NAME}.tmp"));

        // Pre-existing corrupt cache.
        fs::write(&final_path, b"GARBAGE FROM A PRIOR CRASHED RUN").unwrap();

        let g = build_sample_graph();
        g.save(dir.path()).unwrap();

        // Final file must now be the new content (parses as a
        // current-schema cache).
        let bytes = fs::read(&final_path).unwrap();
        let cache: GraphCache = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(cache.version, CACHE_VERSION);
        assert_eq!(cache.generator, GENERATOR);
        assert!(!cache.nodes.is_empty(), "cache must contain nodes");

        // No leftover tmp file: rename consumed it.
        assert!(
            !tmp_path.exists(),
            ".tmp file must not exist after a successful save"
        );
    }

    #[test]
    fn save_does_not_disturb_unrelated_tmp_file() {
        // API-contract assertion: a stray `.tmp` file from some other
        // process or a prior crashed save does not corrupt the final
        // file. After save, the final file is well-formed and matches the
        // graph we serialized.
        let dir = TempDir::new().unwrap();
        let final_path = cache_path(dir.path());
        let tmp_path = dir.path().join(format!("{CACHE_FILE_NAME}.tmp"));

        // Plant a stray (smaller, garbage) tmp file.
        fs::write(&tmp_path, b"stale tmp from a crashed run").unwrap();

        let g = build_sample_graph();
        g.save(dir.path()).unwrap();

        // Final file is well-formed and contains our data.
        let bytes = fs::read(&final_path).unwrap();
        let cache: GraphCache = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(cache.version, CACHE_VERSION);
        assert!(cache.nodes.contains_key("/a.cpp:foo"));

        // Tmp file was overwritten and then renamed — it must be gone now.
        assert!(!tmp_path.exists());
    }

    #[test]
    fn mtime_invalidation_detects_modified_file() {
        // Build a graph whose `files` map references a real file on disk,
        // save, then modify the file. `stale_paths` should flag it.
        let dir = TempDir::new().unwrap();
        let src_path = dir.path().join("a.cpp");
        fs::write(&src_path, b"// initial\n").unwrap();

        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            src_path.to_str().unwrap(),
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, src_path.to_str().unwrap())],
            vec![],
        ));
        g.save(dir.path()).unwrap();

        // OpenOptions::write+truncate is reliable for triggering an mtime
        // bump even on filesystems with second-resolution timestamps,
        // because writes go through the inode update path. We also call
        // `set_modified` if the FS gave us a too-coarse update.
        {
            let mut f = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&src_path)
                .unwrap();
            f.write_all(b"// modified content - longer than initial\n")
                .unwrap();
            f.sync_all().unwrap();
        }
        // Force a distinct mtime via set_modified (stable since 1.75).
        // This sidesteps low-resolution FS timestamps that would otherwise
        // produce identical pre/post nanosecond counts.
        let new_mtime = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
        let f = OpenOptions::new().write(true).open(&src_path).unwrap();
        f.set_modified(new_mtime).unwrap();
        drop(f);

        let stale = stale_paths(dir.path()).unwrap();
        assert_eq!(stale.len(), 1, "exactly one path should be stale");
        assert_eq!(stale[0], src_path);
    }

    #[test]
    fn mtime_invalidation_detects_deleted_file() {
        let dir = TempDir::new().unwrap();
        let src_path = dir.path().join("ghost.cpp");
        fs::write(&src_path, b"// soon to be deleted\n").unwrap();

        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            src_path.to_str().unwrap(),
            Language::Cpp,
            vec![sym(
                "ghost",
                SymbolKind::Function,
                src_path.to_str().unwrap(),
            )],
            vec![],
        ));
        g.save(dir.path()).unwrap();

        fs::remove_file(&src_path).unwrap();

        let stale = stale_paths(dir.path()).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], src_path);
    }

    #[test]
    fn cache_path_returns_correct_filename() {
        let dir = Path::new("/some/dir");
        assert_eq!(
            cache_path(dir),
            Path::new("/some/dir/.code-graph-cache.json")
        );
    }

    #[test]
    fn save_persists_language_in_files_entry() {
        // v2-specific: FileEntry on disk must carry the language tag so
        // load doesn't have to re-derive it from the extension. This is
        // the headline difference from Go's v1 format.
        let dir = TempDir::new().unwrap();
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.py",
            Language::Python,
            vec![sym("snake_case_fn", SymbolKind::Function, "/a.py")],
            vec![],
        ));
        g.save(dir.path()).unwrap();

        let bytes = fs::read(cache_path(dir.path())).unwrap();
        let cache: GraphCache = serde_json::from_slice(&bytes).unwrap();
        let entry = cache
            .files
            .get(&PathBuf::from("/a.py"))
            .expect("file entry present");
        assert_eq!(entry.language, Language::Python);
        assert_eq!(entry.symbol_ids, vec!["/a.py:snake_case_fn".to_string()]);
    }

    // -------------------------------------------------------------------
    // simplify_cache / simplify_symbol_id unit tests.
    //
    // Platform note: `paths::simplify` is identity on non-Windows targets
    // (per `dunce::simplified`'s documented behavior). The "strips the
    // `\\?\` prefix" assertion is therefore only verifiable on Windows —
    // covered by the `#[cfg(windows)]` test in `code_graph_core::paths`.
    //
    // What the tests below verify on Linux (and Windows):
    // - `simplify_symbol_id` splits on the rightmost `:` not part of `::`
    //   (mirrors `id_to_file`'s contract), invokes `paths::simplify` on the
    //   file portion, and rejoins. On Linux this is observable as a
    //   round-trip identity on clean inputs; the structural correctness of
    //   the split/rejoin is the load-bearing assertion.
    // - `simplify_cache` performs a drain-and-rebuild over every map and
    //   leaves a clean cache byte-equal (idempotency).
    //
    // The actual strip is verified end-to-end on Windows; here we test the
    // **shape** of the rewrite, which is what regresses if a future change
    // forgets a field.
    // -------------------------------------------------------------------

    #[test]
    fn simplify_symbol_id_basic() {
        // Free-function id: rightmost qualifying colon is the single
        // separator between file portion and name. On Linux the file
        // portion is identity through `paths::simplify`, so the input
        // round-trips. The test pins the structural rule: split on `:`,
        // simplify file portion, rejoin.
        let id = "/a/b.rs:Foo";
        assert_eq!(simplify_symbol_id(id), "/a/b.rs:Foo");
    }

    #[test]
    fn simplify_symbol_id_with_method_separator() {
        // Method id format: "file:Parent::name". The `::` between Parent
        // and name must NOT be split — only the single colon to the LEFT
        // of `Parent::name` qualifies. On Linux the file portion is
        // identity, so the input round-trips; the assertion pins that the
        // helper did not split at one of the `::` colons.
        let id = "/a/b.rs:Foo::bar";
        assert_eq!(simplify_symbol_id(id), "/a/b.rs:Foo::bar");
    }

    #[test]
    fn simplify_symbol_id_relative_path_round_trips() {
        // Relative Unix-style id (no leading `/`) — pins that the helper
        // doesn't require an absolute path. `paths::simplify` is identity
        // on relative inputs, so the input round-trips. Distinct coverage
        // from `_basic` (absolute) and `_with_method_separator` (`::`).
        let id = "sub/dir/file.rs:helper";
        assert_eq!(simplify_symbol_id(id), id);
    }

    #[test]
    fn simplify_symbol_id_malformed_no_separator_is_identity() {
        // No `:` at all — `id_to_file` returns `""` for this shape and we
        // have no file portion to rewrite; documented behavior is to leave
        // the id untouched.
        assert_eq!(simplify_symbol_id("noseparator"), "noseparator");
    }

    #[test]
    fn simplify_symbol_id_leading_colon_is_identity() {
        // Leading `:` qualifies as a separator per `id_to_file`'s contract
        // (no left neighbor, non-colon right neighbor). The file portion is
        // an empty string; `paths::simplify(Path::new(""))` yields `""` and
        // the rejoin reproduces the original id. We assert the
        // user-visible identity rather than the internal path-through.
        assert_eq!(simplify_symbol_id(":foo"), ":foo");
    }

    #[test]
    fn simplify_symbol_id_unix_filename_with_colon() {
        // Filename containing `:` (legal on Unix). The rightmost
        // qualifying colon is the separator between `bar.rs` and `func`;
        // the `:` inside the filename must be preserved in the file
        // portion.
        assert_eq!(
            simplify_symbol_id("/project/foo:bar.rs:func"),
            "/project/foo:bar.rs:func"
        );
    }

    #[test]
    fn simplify_symbol_id_windows_drive_letter_path() {
        // Windows-style id with a drive-letter colon at index 1. The
        // rightmost qualifying colon (the one between `b.cs` and `Baz`) is
        // found first by the right-to-left scan; the drive colon is never
        // visited as a separator candidate. This pins the disambiguation
        // contract — a future algorithm change that incorrectly identifies
        // the drive colon as the separator would corrupt every Windows
        // symbol's id during cache migration.
        //
        // On Linux `paths::simplify` is identity, so the assertion is
        // structural (split + rejoin produces the input). On Windows the
        // bare-drive-letter form `C:\...` is also short-form, so
        // `paths::simplify` is also identity — the structural correctness
        // is what matters; this is NOT a Windows-only test.
        assert_eq!(
            simplify_symbol_id(r"C:\a\b.cs:Baz::qux"),
            r"C:\a\b.cs:Baz::qux"
        );
    }

    #[test]
    fn simplify_symbol_id_only_double_colons_is_identity() {
        // Pathological input: every `:` is part of a `::` pair, so no
        // qualifying separator is found and the fallback returns the input
        // unchanged. Mirrors `id_to_file`'s `only_double_colons` boundary
        // case (see `code_graph_core::id_to_file` tests).
        assert_eq!(simplify_symbol_id("::"), "::");
        assert_eq!(simplify_symbol_id("foo::bar"), "foo::bar");
    }

    /// Build a `GraphCache` exercising every path-bearing field, using the
    /// supplied `file_path` string as the planted file path everywhere a
    /// path appears. Returns the cache plus the SymbolId strings that
    /// were planted (for use in cross-field consistency assertions).
    ///
    /// Two symbols are planted: a free function and a method on a class
    /// (so the method's SymbolId carries `::`, exercising the
    /// `simplify_symbol_id` split rule).
    fn build_cache_with_path(file_path: &str) -> GraphCache {
        let path_buf = PathBuf::from(file_path);
        let dep_path_buf = PathBuf::from(format!("{file_path}.dep"));
        let other_path_buf = PathBuf::from(format!("{file_path}.other"));

        let free_id: SymbolId = format!("{file_path}:Foo");
        let method_id: SymbolId = format!("{file_path}:Bar::baz");
        let target_id: SymbolId = format!("{file_path}:Quux");

        let free_symbol = Symbol {
            name: "Foo".to_string(),
            kind: SymbolKind::Function,
            file: file_path.to_string(),
            line: 1,
            column: 0,
            end_line: 3,
            signature: "fn Foo()".to_string(),
            namespace: String::new(),
            parent: String::new(),
            language: Language::Rust,
        };
        let method_symbol = Symbol {
            name: "baz".to_string(),
            kind: SymbolKind::Method,
            file: file_path.to_string(),
            line: 5,
            column: 0,
            end_line: 7,
            signature: "fn baz()".to_string(),
            namespace: String::new(),
            parent: "Bar".to_string(),
            language: Language::Rust,
        };

        let mut nodes = HashMap::new();
        nodes.insert(free_id.clone(), free_symbol);
        nodes.insert(method_id.clone(), method_symbol);

        let edge_entry = EdgeEntry {
            target: target_id.clone(),
            kind: EdgeKind::Calls,
            file: path_buf.clone(),
            line: 2,
        };
        let mut adj = HashMap::new();
        adj.insert(free_id.clone(), vec![edge_entry.clone()]);
        let mut radj = HashMap::new();
        radj.insert(method_id.clone(), vec![edge_entry]);

        let mut files = HashMap::new();
        files.insert(
            path_buf.clone(),
            FileEntry {
                language: Language::Rust,
                symbol_ids: vec![free_id, method_id],
            },
        );

        let mut includes = HashMap::new();
        // Distinctive non-zero lines so the round-trip / migration tests
        // prove `IncludeEntry.line` survives serialization and the
        // path-simplify pass (which must leave `line` untouched).
        includes.insert(
            path_buf.clone(),
            vec![
                IncludeEntry {
                    path: dep_path_buf.clone(),
                    line: 11,
                },
                IncludeEntry {
                    path: other_path_buf.clone(),
                    line: 22,
                },
            ],
        );

        let mut mtimes = HashMap::new();
        mtimes.insert(path_buf, 42);
        mtimes.insert(dep_path_buf, 7);

        GraphCache {
            version: CACHE_VERSION,
            generator: GENERATOR.to_string(),
            nodes,
            adj,
            radj,
            files,
            includes,
            mtimes,
            last_sweep_at: 0,
        }
    }

    #[test]
    fn simplify_cache_strips_all_fields() {
        // On Linux, `paths::simplify` is identity, so this test verifies
        // the **structural** drain-and-rebuild over every field: a clean
        // input produces a byte-equal clean output (one assertion below
        // covers this via idempotency), AND every individual location
        // round-trips through the rewrite without dropping data or
        // corrupting shape. On Windows the same drain-and-rebuild path
        // would additionally strip `\\?\` prefixes; the Windows-specific
        // strip is pinned by the `#[cfg(windows)]` tests in
        // `code_graph_core::paths`.
        //
        // We use a path that, on Windows, `paths::simplify` would
        // transform (the verbatim disk form), and check **structurally**
        // that every field's contents are equal to `paths::simplify`
        // applied to the original — so the test asserts the right thing
        // regardless of platform identity-vs-strip behavior.
        let planted = r"\\?\D:\proj\file.h";
        let mut cache = build_cache_with_path(planted);

        simplify_cache(&mut cache);

        let expected_path = paths::simplify(Path::new(planted));
        let expected_path_str = expected_path.to_string_lossy().into_owned();
        let expected_free_id = format!("{expected_path_str}:Foo");
        let expected_method_id = format!("{expected_path_str}:Bar::baz");
        let expected_target_id = format!("{expected_path_str}:Quux");
        let expected_dep = paths::simplify(Path::new(&format!("{planted}.dep")));
        let expected_other = paths::simplify(Path::new(&format!("{planted}.other")));

        // (1) files map keys.
        assert!(
            cache.files.contains_key(&expected_path),
            "files key not simplified: keys={:?}",
            cache.files.keys().collect::<Vec<_>>()
        );
        // (2) FileEntry.symbol_ids entries (both free and method form).
        let entry = cache
            .files
            .get(&expected_path)
            .expect("file entry present after simplify");
        assert!(
            entry.symbol_ids.contains(&expected_free_id),
            "FileEntry.symbol_ids missing simplified free id: {:?}",
            entry.symbol_ids
        );
        assert!(
            entry.symbol_ids.contains(&expected_method_id),
            "FileEntry.symbol_ids missing simplified method id: {:?}",
            entry.symbol_ids
        );

        // (3) nodes map keys.
        assert!(
            cache.nodes.contains_key(&expected_free_id),
            "nodes key (free) not simplified: keys={:?}",
            cache.nodes.keys().collect::<Vec<_>>()
        );
        assert!(
            cache.nodes.contains_key(&expected_method_id),
            "nodes key (method) not simplified: keys={:?}",
            cache.nodes.keys().collect::<Vec<_>>()
        );
        // (4) Symbol.file values on each node.
        let free_symbol = cache.nodes.get(&expected_free_id).unwrap();
        assert_eq!(
            free_symbol.file, expected_path_str,
            "Symbol.file (free) not simplified"
        );
        let method_symbol = cache.nodes.get(&expected_method_id).unwrap();
        assert_eq!(
            method_symbol.file, expected_path_str,
            "Symbol.file (method) not simplified"
        );

        // (5) adj map keys.
        assert!(
            cache.adj.contains_key(&expected_free_id),
            "adj key not simplified: keys={:?}",
            cache.adj.keys().collect::<Vec<_>>()
        );
        let adj_entries = cache.adj.get(&expected_free_id).unwrap();
        assert_eq!(adj_entries.len(), 1, "adj vec length preserved");
        // (6) EdgeEntry.target SymbolId.
        assert_eq!(
            adj_entries[0].target, expected_target_id,
            "adj EdgeEntry.target not simplified"
        );
        // (7) EdgeEntry.file PathBuf.
        assert_eq!(
            adj_entries[0].file, expected_path,
            "adj EdgeEntry.file not simplified"
        );

        // (8) radj map keys.
        assert!(
            cache.radj.contains_key(&expected_method_id),
            "radj key not simplified: keys={:?}",
            cache.radj.keys().collect::<Vec<_>>()
        );
        let radj_entries = cache.radj.get(&expected_method_id).unwrap();
        // (9) EdgeEntry.target inside radj.
        assert_eq!(
            radj_entries[0].target, expected_target_id,
            "radj EdgeEntry.target not simplified"
        );
        // (10) EdgeEntry.file inside radj.
        assert_eq!(
            radj_entries[0].file, expected_path,
            "radj EdgeEntry.file not simplified"
        );

        // (11) includes map keys AND inner IncludeEntry.path values; the
        // `line` field must pass through the simplify unchanged.
        assert!(
            cache.includes.contains_key(&expected_path),
            "includes key not simplified: keys={:?}",
            cache.includes.keys().collect::<Vec<_>>()
        );
        let inner = cache.includes.get(&expected_path).unwrap();
        assert!(
            inner.iter().any(|e| e.path == expected_dep && e.line == 11),
            "includes inner Vec missing simplified dep with preserved line: {inner:?}"
        );
        assert!(
            inner
                .iter()
                .any(|e| e.path == expected_other && e.line == 22),
            "includes inner Vec missing simplified other with preserved line: {inner:?}"
        );

        // (12) mtimes map keys.
        assert!(
            cache.mtimes.contains_key(&expected_path),
            "mtimes key not simplified: keys={:?}",
            cache.mtimes.keys().collect::<Vec<_>>()
        );
        assert!(
            cache.mtimes.contains_key(&expected_dep),
            "mtimes second key not simplified: keys={:?}",
            cache.mtimes.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn simplify_cache_idempotent_on_clean_input() {
        // Build a cache whose paths are already "clean" (no extended
        // prefix). `simplify_cache` must produce a by-value-equal cache:
        // every map key and value identical, every Vec ordered the same.
        //
        // Reserialize-and-compare via JSON is the strongest byte-identity
        // check available: HashMap iteration order is non-deterministic
        // for serde_json's default object output, but `BTreeMap`-free
        // serde of `HashMap` still produces a JSON object whose KEYS are
        // unordered yet whose VALUES round-trip identically. We compare
        // by value-equality on the cache fields directly — which catches
        // any rewrite that drops or duplicates an entry — and separately
        // assert the JSON deserializes back to the same in-memory shape.
        let before = build_cache_with_path("/a/b.rs");

        // Clone via JSON round-trip to get an independent before-state we
        // can compare against (the cache fields don't implement Clone
        // directly but the serde derive gives us this for free).
        let before_json = serde_json::to_value(&before).unwrap();

        let mut after = build_cache_with_path("/a/b.rs");
        simplify_cache(&mut after);
        let after_json = serde_json::to_value(&after).unwrap();

        // Field-by-field value equality. `serde_json::Value` Eq compares
        // object contents by key, not insertion order — ideal for HashMap
        // round-trips.
        assert_eq!(
            before_json, after_json,
            "simplify_cache on clean input must produce an equal cache"
        );

        // Running it a second time on the already-clean output must also
        // be idempotent — defends against a rewrite that happens to be
        // idempotent only on the first pass.
        simplify_cache(&mut after);
        let after_twice = serde_json::to_value(&after).unwrap();
        assert_eq!(
            before_json, after_twice,
            "simplify_cache must be idempotent under repeated application"
        );
    }

    #[test]
    fn round_trip_preserves_inherits_edge_kind() {
        // Defends against a future edge-kind serialization regression: an
        // Inherits edge must round-trip through JSON unchanged.
        let dir = TempDir::new().unwrap();
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("Base", SymbolKind::Class, "/a.cpp"),
                sym("Derived", SymbolKind::Class, "/a.cpp"),
            ],
            vec![inherit_edge("Derived", "Base", "/a.cpp")],
        ));
        g.save(dir.path()).unwrap();

        let mut loaded = Graph::new();
        assert!(loaded.load(dir.path()).unwrap());
        let edge = &loaded.adj["Derived"][0];
        assert_eq!(edge.target, "Base");
        assert_eq!(edge.kind, EdgeKind::Inherits);
    }

    // -------------------------------------------------------------------
    // End-to-end anti-regression test for `Graph::load` wiring.
    //
    // The `simplify_cache_strips_all_fields` unit test exercises
    // `simplify_cache` DIRECTLY — i.e., without the disk write or the
    // `Graph::load` plumbing. This test covers the END-TO-END path:
    //   (1) build a `GraphCache` with `\\?\D:\proj\…` in every path-bearing
    //       location,
    //   (2) write it to a tempdir as cache JSON (skipping `Graph::save`,
    //       which would simplify-on-the-way-out via the in-memory graph's
    //       already-clean state),
    //   (3) call `Graph::load` and assert the loaded graph is well-formed.
    //
    // Platform behavior — and the load-bearing caveat for this test:
    // - On non-Windows (Linux/macOS): `paths::simplify` is identity per
    //   `dunce::simplified`'s contract. The planted `\\?\` strings flow
    //   through `simplify_cache` UNCHANGED. **Limitation:** on Linux, this
    //   test cannot distinguish "`simplify_cache` was called and was a
    //   no-op" from "`simplify_cache` was never called" — both produce
    //   identical output. The structural and per-location assertions catch
    //   regressions that drop, corrupt, or duplicate fields (because those
    //   change graph shape regardless of strip behavior), but a pure
    //   deletion of the `simplify_cache(&mut cache)` call from
    //   `Graph::load` would still pass on Linux. The 2.1 unit test
    //   (`simplify_cache_strips_all_fields`) is the function-body contract;
    //   this test is the Graph::load wiring contract — observable only on
    //   Windows.
    // - On Windows: `paths::simplify` strips `\\?\`. The test asserts each
    //   of the 12 planted locations has been stripped, with one assertion
    //   per location naming the offending field on failure (the explicit
    //   regression target — partial migration must fail loudly). A
    //   `#[cfg(windows)]` ground-truth check below also asserts that
    //   `paths::simplify` actually performed observable work on the test
    //   fixture; if `paths::simplify` itself regresses to identity, the
    //   ground-truth check fires before any of the per-location assertions.
    //
    // The `mtimes` field is not exposed on `Graph` (it lives only on
    // `GraphCache`); its rewrite is covered by the 2.1 in-memory unit test
    // (`simplify_cache_strips_all_fields`). End-to-end coverage of `mtimes`
    // through `Graph::load` is partial: a `simplify_cache` bug in the
    // mtimes block that panics or corrupts deserialization would surface
    // here (because `Graph::load` would fail). A silent correctness bug in
    // the mtimes block (wrong key written, entry dropped without panic)
    // would NOT be detected — `mtimes` is never read back from the
    // resulting `Graph`. Future closing of this gap would require either a
    // crate-internal `#[cfg(test)]` accessor for `mtimes` or routing
    // `stale_paths` through the loaded graph state.
    // -------------------------------------------------------------------

    #[test]
    fn cache_migration_strips_all_path_locations_end_to_end() {
        // Plant `\\?\D:\proj\file.h` in every path-bearing location of a
        // synthetic `GraphCache`. The string is intentionally a Windows
        // verbatim-extended path: on Windows `paths::simplify` strips it to
        // `D:\proj\file.h`; on Linux `paths::simplify` is identity so the
        // planted string survives. Both outcomes are valid migration output
        // — the test asserts the platform-appropriate one.
        let planted = r"\\?\D:\proj\file.h";
        let cache = build_cache_with_path(planted);
        // Sanity: the fixture below is what `build_cache_with_path` plants.
        // Asserting the shape explicitly (rather than re-reading
        // `cache.nodes.len()` etc.) documents what the test expects to find
        // post-load and fails loudly if `build_cache_with_path` is later
        // shrunk in a way that hides a regression.
        assert_eq!(
            cache.nodes.len(),
            2,
            "fixture invariant: 2 nodes (free fn + method)"
        );
        assert_eq!(cache.files.len(), 1, "fixture invariant: 1 file entry");
        assert_eq!(
            cache.adj.len(),
            1,
            "fixture invariant: 1 adj key (free fn source)"
        );
        assert_eq!(
            cache.radj.len(),
            1,
            "fixture invariant: 1 radj key (method target)"
        );
        assert_eq!(cache.includes.len(), 1, "fixture invariant: 1 includes key");

        // Write the synthetic cache directly via `serde_json::to_vec` +
        // `fs::write` — bypassing `Graph::save`, which would serialize the
        // current in-memory state (empty / already-clean) rather than our
        // planted dirty cache.
        let dir = TempDir::new().unwrap();
        let bytes = serde_json::to_vec(&cache).unwrap();
        fs::write(cache_path(dir.path()), &bytes).unwrap();

        // Load via the production path. This is the wiring under test:
        // `Graph::load` must call `simplify_cache` between deserialize and
        // materialize.
        let mut graph = Graph::new();
        let loaded = graph.load(dir.path()).expect("load must not error");
        assert!(
            loaded,
            "Graph::load must return Ok(true) for a current-version cache — \
             a version-mismatch silent re-index would mean the migration \
             never ran"
        );

        // Compute the expected post-migration strings. On Linux these are
        // byte-identical to the planted strings (identity); on Windows the
        // `\\?\` prefix has been stripped. We compute them through
        // `paths::simplify` so the assertions remain correct on both
        // platforms.
        let expected_path = paths::simplify(Path::new(planted));
        let expected_path_str = expected_path.to_string_lossy().into_owned();
        let expected_free_id: SymbolId = format!("{expected_path_str}:Foo");
        let expected_method_id: SymbolId = format!("{expected_path_str}:Bar::baz");
        let expected_target_id: SymbolId = format!("{expected_path_str}:Quux");
        let expected_dep = paths::simplify(Path::new(&format!("{planted}.dep")));
        let expected_other = paths::simplify(Path::new(&format!("{planted}.other")));

        // ---------- Structural assertions (cross-platform) ----------
        //
        // These prove `Graph::load`'s wiring did NOT drop or corrupt any
        // field while running `simplify_cache`. They fire on every
        // platform; a structural regression (e.g. `simplify_cache` dropping
        // a HashMap entry) fails these regardless of identity-vs-strip
        // behavior.

        assert_eq!(
            graph.nodes.len(),
            2,
            "loaded graph: 2 nodes (free fn + method)"
        );
        assert_eq!(graph.files.len(), 1, "loaded graph: 1 file entry");
        assert_eq!(graph.adj.len(), 1, "loaded graph: 1 adj key");
        assert_eq!(graph.radj.len(), 1, "loaded graph: 1 radj key");
        assert_eq!(graph.includes.len(), 1, "loaded graph: 1 includes key");

        // ---------- Per-location assertions ----------
        //
        // Each of the 12 granular path-bearing locations is asserted
        // independently. On Linux these confirm the planted
        // strings survived `simplify_cache` unchanged; on Windows they
        // confirm each `\\?\` prefix was stripped. The expected strings
        // are computed via `paths::simplify` so the same assertion holds
        // on both platforms.
        //
        // Each assertion message names the offending location so a
        // future partial-migration regression points directly at the field
        // that was missed.

        // (1) `files` map key.
        assert!(
            graph.files.contains_key(expected_path.as_path()),
            "files map key not migrated: keys={:?}",
            graph.files.keys().collect::<Vec<_>>()
        );

        // (2) `FileEntry.symbol_ids` — both free and method form.
        let file_entry = graph
            .files
            .get(expected_path.as_path())
            .expect("file entry present after load");
        assert!(
            file_entry.symbol_ids.contains(&expected_free_id),
            "FileEntry.symbol_ids missing migrated free id ({expected_free_id:?}): \
             actual={:?}",
            file_entry.symbol_ids
        );
        assert!(
            file_entry.symbol_ids.contains(&expected_method_id),
            "FileEntry.symbol_ids missing migrated method id ({expected_method_id:?}): \
             actual={:?}",
            file_entry.symbol_ids
        );

        // (3) `nodes` map keys — both free and method form.
        assert!(
            graph.nodes.contains_key(&expected_free_id),
            "nodes map missing migrated free key ({expected_free_id:?}): keys={:?}",
            graph.nodes.keys().collect::<Vec<_>>()
        );
        assert!(
            graph.nodes.contains_key(&expected_method_id),
            "nodes map missing migrated method key ({expected_method_id:?}): keys={:?}",
            graph.nodes.keys().collect::<Vec<_>>()
        );

        // (4) `Symbol.file` field on each Node's wrapped Symbol.
        let free_node = graph.nodes.get(&expected_free_id).unwrap();
        assert_eq!(
            free_node.symbol.file, expected_path_str,
            "Node.symbol.file (free) not migrated"
        );
        let method_node = graph.nodes.get(&expected_method_id).unwrap();
        assert_eq!(
            method_node.symbol.file, expected_path_str,
            "Node.symbol.file (method) not migrated"
        );

        // (5) `adj` map key.
        assert!(
            graph.adj.contains_key(&expected_free_id),
            "adj map missing migrated key ({expected_free_id:?}): keys={:?}",
            graph.adj.keys().collect::<Vec<_>>()
        );
        let adj_entries = graph.adj.get(&expected_free_id).unwrap();
        assert_eq!(
            adj_entries.len(),
            1,
            "adj vec length preserved through load"
        );

        // (6) `EdgeEntry.target` in `adj`.
        assert_eq!(
            adj_entries[0].target, expected_target_id,
            "adj EdgeEntry.target not migrated"
        );

        // (7) `EdgeEntry.file` in `adj`.
        assert_eq!(
            adj_entries[0].file, expected_path,
            "adj EdgeEntry.file not migrated"
        );

        // (8) `radj` map key.
        assert!(
            graph.radj.contains_key(&expected_method_id),
            "radj map missing migrated key ({expected_method_id:?}): keys={:?}",
            graph.radj.keys().collect::<Vec<_>>()
        );
        let radj_entries = graph.radj.get(&expected_method_id).unwrap();
        assert_eq!(
            radj_entries.len(),
            1,
            "radj vec length preserved through load"
        );

        // (9) `EdgeEntry.target` in `radj`.
        assert_eq!(
            radj_entries[0].target, expected_target_id,
            "radj EdgeEntry.target not migrated"
        );

        // (10) `EdgeEntry.file` in `radj`.
        assert_eq!(
            radj_entries[0].file, expected_path,
            "radj EdgeEntry.file not migrated"
        );

        // (11a) `includes` map key.
        assert!(
            graph.includes.contains_key(expected_path.as_path()),
            "includes map missing migrated key ({expected_path:?}): keys={:?}",
            graph.includes.keys().collect::<Vec<_>>()
        );
        let includes_inner = graph.includes.get(expected_path.as_path()).unwrap();
        assert_eq!(
            includes_inner.len(),
            2,
            "includes inner Vec: 2 deps (.dep + .other)"
        );

        // (11b) inner `IncludeEntry` entries in `includes`: the path is
        // migrated and the source line round-trips through load.
        assert!(
            includes_inner
                .iter()
                .any(|e| e.path == expected_dep && e.line == 11),
            "includes inner Vec missing migrated dep ({expected_dep:?}) with preserved line: \
             actual={includes_inner:?}"
        );
        assert!(
            includes_inner
                .iter()
                .any(|e| e.path == expected_other && e.line == 22),
            "includes inner Vec missing migrated other ({expected_other:?}) with preserved line: \
             actual={includes_inner:?}"
        );

        // (12) `mtimes` rewrite is not directly observable here: `Graph`
        // exposes no `mtimes` accessor, so this test cannot assert the
        // migrated `mtimes` keys. A `simplify_cache` bug that panics or
        // breaks deserialization in the mtimes block WOULD surface here
        // (the `Graph::load` call above would fail), but a silent
        // correctness bug (wrong key, dropped entry) would NOT. The 2.1
        // unit test (`simplify_cache_strips_all_fields`) is the authoritative
        // contract for the `mtimes` rewrite shape.

        // ---------- Windows-only strip assertions ----------
        //
        // Ground-truth check FIRST: if `paths::simplify` itself regresses
        // to identity on Windows, every per-location assertion above would
        // still trivially pass (expected == planted on both sides). Pin
        // the substantive behavior here so a `dunce` swap or signature
        // change surfaces before the per-location checks.
        //
        // The sweep block after the ground-truth check is belt-and-
        // suspenders for future fixture growth: with the current 1-entry
        // map fixtures it is fully redundant with the per-location
        // assertions above. If a future contributor adds more entries to
        // `build_cache_with_path` without adding matching per-location
        // assertions, the sweep catches a partial-migration regression
        // that the per-location coverage would miss.
        #[cfg(windows)]
        {
            assert_eq!(
                expected_path_str, r"D:\proj\file.h",
                "ground truth: paths::simplify must strip \\?\\ on Windows. \
                 If this fails, the per-location assertions below are vacuous \
                 (expected == planted) and the test no longer protects the \
                 migration contract."
            );
            let prefix = r"\\?\";
            for key in graph.files.keys() {
                assert!(
                    !key.to_string_lossy().contains(prefix),
                    "files map key still contains verbatim prefix: {key:?}"
                );
            }
            for entry in graph.files.values() {
                for sid in &entry.symbol_ids {
                    assert!(
                        !sid.contains(prefix),
                        "FileEntry.symbol_ids entry still contains verbatim prefix: {sid:?}"
                    );
                }
            }
            for key in graph.nodes.keys() {
                assert!(
                    !key.contains(prefix),
                    "nodes map key still contains verbatim prefix: {key:?}"
                );
            }
            for node in graph.nodes.values() {
                assert!(
                    !node.symbol.file.contains(prefix),
                    "Symbol.file still contains verbatim prefix: {:?}",
                    node.symbol.file
                );
            }
            for (key, entries) in &graph.adj {
                assert!(
                    !key.contains(prefix),
                    "adj map key still contains verbatim prefix: {key:?}"
                );
                for entry in entries {
                    assert!(
                        !entry.target.contains(prefix),
                        "adj EdgeEntry.target still contains verbatim prefix: {:?}",
                        entry.target
                    );
                    assert!(
                        !entry.file.to_string_lossy().contains(prefix),
                        "adj EdgeEntry.file still contains verbatim prefix: {:?}",
                        entry.file
                    );
                }
            }
            for (key, entries) in &graph.radj {
                assert!(
                    !key.contains(prefix),
                    "radj map key still contains verbatim prefix: {key:?}"
                );
                for entry in entries {
                    assert!(
                        !entry.target.contains(prefix),
                        "radj EdgeEntry.target still contains verbatim prefix: {:?}",
                        entry.target
                    );
                    assert!(
                        !entry.file.to_string_lossy().contains(prefix),
                        "radj EdgeEntry.file still contains verbatim prefix: {:?}",
                        entry.file
                    );
                }
            }
            for (key, deps) in &graph.includes {
                assert!(
                    !key.to_string_lossy().contains(prefix),
                    "includes map key still contains verbatim prefix: {key:?}"
                );
                for dep in deps {
                    assert!(
                        !dep.to_string_lossy().contains(prefix),
                        "includes inner Vec entry still contains verbatim prefix: {dep:?}"
                    );
                }
            }
        }
    }

    // -------------------------------------------------------------------
    // End-to-end cross-field consistency test for `Graph::load`.
    //
    // The `cache_migration_strips_all_path_locations_end_to_end` test
    // verifies each path-bearing field is migrated in isolation. This test
    // is the **user-experience** check: does a real lookup against a
    // migrated cache return the expected symbols?
    //
    // The lookup path inside `Graph::file_symbols` (crates/code-graph-graph/
    // src/queries.rs:150) is:
    //
    //   self.files[<path>].symbol_ids  →  self.nodes[<id>]  →  Symbol
    //
    // If 2.1 missed simplifying ANY of these — say, `nodes` map keys remain
    // prefixed while `files` keys are stripped — then `file_symbols` returns
    // an empty `Vec` for a short-form lookup, EVEN THOUGH 2.3's per-location
    // assertions might still pass for each field viewed independently. That
    // is the failure mode this test pins.
    //
    // Platform behavior:
    // - On Linux: `paths::simplify` is identity. The planted `\\?\`-prefixed
    //   strings flow through `Graph::load` unchanged, and the lookup must
    //   use the same planted string (re-simplified to itself) as the key.
    //   The test still validates cross-field alignment: if the migration
    //   internally rewrote one field but not another, the lookup would
    //   return empty even on Linux.
    // - On Windows: `paths::simplify` strips `\\?\`. The lookup key is
    //   `D:\proj\file.h`; the migrated cache's `files` key, `nodes` SymbolId
    //   prefix, and `Symbol.file` value all equal `D:\proj\file.h`. The test
    //   validates both strip correctness AND cross-field alignment.
    //
    // We use `paths::simplify(Path::new(planted))` to compute the lookup key
    // — NOT a hardcoded `D:\proj\file.h` — so the test is meaningful on
    // both platforms.
    // -------------------------------------------------------------------

    #[test]
    fn cache_migration_preserves_cross_field_consistency() {
        // Plant `\\?\D:\proj\file.h` in every path-bearing location. After
        // `Graph::load` (which runs `simplify_cache`), every reference to
        // this path across all fields must agree on its post-migration form
        // — otherwise the lookup chain through `file_symbols` breaks.
        let planted = r"\\?\D:\proj\file.h";
        let cache = build_cache_with_path(planted);

        // Write the synthetic cache directly via `serde_json::to_vec` +
        // `fs::write` (same idiom as 2.3) — bypassing `Graph::save`, which
        // would serialize the current in-memory state rather than our
        // planted dirty cache.
        let dir = TempDir::new().unwrap();
        let bytes = serde_json::to_vec(&cache).unwrap();
        fs::write(cache_path(dir.path()), &bytes).unwrap();

        let mut graph = Graph::new();
        let loaded = graph.load(dir.path()).expect("load must not error");
        assert!(
            loaded,
            "Graph::load must return Ok(true) for a current-version cache — \
             a silent re-index would mean the migration never ran"
        );

        // Compute the expected post-migration path. On Linux this is
        // identity (the planted `\\?\…` survives); on Windows the `\\?\`
        // prefix is stripped to `D:\proj\file.h`. The test uses this
        // platform-conditional expected path as the lookup key, NOT a
        // hardcoded `D:\proj\file.h` (which would only work on Windows).
        // This is the cross-FIELD consistency check — what matters is that
        // the graph internally aligns its keys after migration, which is
        // observable on both platforms via the lookup-then-assert chain.
        let lookup_path = paths::simplify(Path::new(planted));
        let expected_path_str = lookup_path.to_string_lossy().into_owned();

        // Ground-truth check on Windows: if `paths::simplify` regresses to
        // identity, the per-platform branch above silently makes this test
        // vacuous (lookup_path == planted on both sides). The assert pins
        // the substantive Windows behavior so a `dunce` swap or signature
        // change surfaces before the cross-field assertions below.
        #[cfg(windows)]
        assert_eq!(
            expected_path_str, r"D:\proj\file.h",
            "ground truth: paths::simplify must strip \\?\\ on Windows. \
             If this fails, the lookup below is checking that the GRAPH \
             still contains the planted dirty path — a vacuous pass."
        );

        // The headline assertion: a `file_symbols` lookup against the
        // migrated cache returns a non-empty result for the expected
        // post-migration path. If any link in the
        // `files → symbol_ids → nodes` chain was missed by `simplify_cache`,
        // this returns an empty Vec.
        let symbols = graph.file_symbols(lookup_path.as_path());
        assert!(
            !symbols.is_empty(),
            "file_symbols({lookup_path:?}) returned empty — \
             cross-field key alignment broken. files keys, FileEntry.symbol_ids, \
             nodes keys, and Symbol.file must all agree on the migrated path. \
             graph.files keys: {:?}; graph.nodes keys: {:?}",
            graph.files.keys().collect::<Vec<_>>(),
            graph.nodes.keys().collect::<Vec<_>>()
        );

        // Find the planted `Foo` symbol in the result. Both planted nodes
        // (`Foo` free fn + `Bar::baz` method) live in the same file, so
        // `file_symbols` returns both; we assert the headline `Foo` symbol
        // is present and has a fully-migrated `file` field.
        let foo = symbols.iter().find(|s| s.name == "Foo").unwrap_or_else(|| {
            panic!(
                "file_symbols returned a non-empty Vec but no Symbol named 'Foo'. \
                     names={:?}",
                symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
            )
        });
        assert_eq!(
            foo.file, expected_path_str,
            "Symbol.file must equal the migrated path (no \\\\?\\ prefix on Windows); \
             a mismatch here means `simplify_cache` rewrote a key but not the value, \
             which would break any caller that compares `Symbol.file` against a \
             short-form path elsewhere in the system"
        );

        // Belt-and-suspenders: also verify the method symbol survives the
        // same lookup. If `simplify_symbol_id` mishandled the `::` rule, the
        // method's SymbolId could have been rewritten differently than its
        // `FileEntry.symbol_ids` entry, dropping it from `file_symbols`
        // output even though the file-level lookup itself succeeded.
        let baz = symbols
            .iter()
            .find(|s| s.name == "baz")
            .expect("method symbol 'baz' must survive file_symbols lookup");
        assert_eq!(
            baz.file, expected_path_str,
            "method Symbol.file must equal the migrated path"
        );
    }
}
