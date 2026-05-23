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

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::graph::Graph;

pub mod packed;

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
// v6: interned + columnar-ready (see Designs/PackedCache). Replaces the
// full-strings-everywhere v4 layout. Path bytes are stored exactly once
// in a `paths` table and referenced by `u32` PathId throughout; symbol
// name / namespace / parent / SymbolId strings are interned via a
// `names` table the same way. On UE/LLVM-scale codebases this is
// expected to shrink the cache by ~3-5× before any binary-format work
// (Phase C). The schema change is non-backward-compatible; v4 caches
// fail the version check below and trigger silent re-index per the
// long-standing contract.
const CACHE_VERSION: u32 = 6;

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

// v4 DTO machinery (GraphCache / GraphCacheRef / StalePathsCache /
// serialize_nodes_as_symbols / simplify_*) deleted in the Phase B v6
// switchover. The v6 path lives entirely in [`packed`]. To recover the
// v4 implementation, see git history at or before commit `90b8d29`
// (`feat(path-trie): add code-graph-path-trie v0.1`).

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

        // Build the v6 packed cache. The encoder walks every internal
        // map, interns paths + name strings into compact tables, and
        // emits the packed DTO. Mtimes are computed fresh from disk
        // inside the encoder (files that no longer exist record `0` so
        // a future `stale_paths` call flags them).
        //
        // Phase B is intentionally still JSON on the wire — Phase C
        // swaps to rkyv with the same encoder feeding it. The encode
        // step's heap cost is bounded by the live graph's size (one
        // u32-per-string-occurrence overhead vs. the prior `clone`
        // path's full copy of every map) — net win, not regression.
        let cache = packed::encode(self, 0);

        // Write-tmp → flush BufWriter → sync_all → rename. The braces
        // matter: `sync_all` must run while the `File` is still open.
        // Atomic-rename contract identical to v4: the tmp file may be
        // left behind on serialization failure but the final cache is
        // never partially overwritten.
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
        // `fs::read`. The buffered reader amortizes `from_reader`'s
        // per-byte dispatch to ~negligible while the reader's own heap
        // footprint stays trivial.
        let reader = io::BufReader::with_capacity(256 * 1024, file);
        let cache: packed::PackedCacheV6 = match serde_json::from_reader(reader) {
            Ok(c) => c,
            // Schema mismatch (a v4/v5 cache, a corrupted file, or any
            // pre-v6 shape) returns Ok(false) so the caller re-indexes.
            // The v6 PackedCacheV6 has a `version` field at the top
            // level whose deserialization is gated by overall JSON
            // structure validity, so a v4 cache fails here OR at the
            // version check below depending on which is reached first.
            Err(_) => return Ok(false),
        };
        if cache.version != CACHE_VERSION {
            return Ok(false);
        }

        let parts = match packed::decode(cache) {
            Ok(p) => p,
            // Decode failure means a v6 cache that's structurally
            // corrupt (e.g. dangling NameId / PathId). Same silent
            // re-index path; treat as cache-absent.
            Err(_) => return Ok(false),
        };

        self.nodes = parts.nodes;
        self.adj = parts.adj;
        self.radj = parts.radj;
        self.files = parts.files;
        self.includes = parts.includes;
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
    // v6 slim-DTO path. `StalePathsCacheV6` deserializes only `paths`
    // + `mtimes` (the rest of the cache fields are skipped by serde's
    // default unknown-field-ignore). Mtimes are stored keyed by PathId
    // u32 in v6 — we resolve each one back to its PathBuf via the
    // `paths` table before doing the on-disk stat compare.
    //
    // Streaming reader + slim DTO. The prior `fs::read` →
    // `from_slice::<GraphCache>` path allocated the full graph
    // (~3-4 GB on a multi-million-symbol cache) only to read one field
    // off it. `StalePathsCacheV6` deserializes `paths` + `mtimes`
    // alone; the rest of the JSON is still parsed by `serde_json`'s
    // stream — there is no JSON-level skip — but the un-mentioned
    // fields are dropped on the floor rather than materialized into
    // HashMaps. Shape-stable across every `CACHE_VERSION` bump so
    // far (mtimes has carried the same `HashMap<PathBuf, u64>` shape
    // since v1).
    let reader = io::BufReader::with_capacity(256 * 1024, file);
    let cache: packed::StalePathsCacheV6 = match serde_json::from_reader(reader) {
        Ok(c) => c,
        // Failed parse (v4 cache, corrupted, anything pre-v6) — surface
        // as "no stale paths"; the caller will redo `Graph::load` and
        // hit the silent-re-index path.
        Err(_) => return Ok(Vec::new()),
    };

    // v6: mtimes is keyed by PathId u32 — resolve to PathBuf via the
    // `paths` table, then stat-and-compare. PathId resolution failures
    // (corrupt cache, out-of-range) are silently dropped — the affected
    // file simply won't be reported stale, the full re-index path
    // catches anything genuinely missing.
    let mut stale = Vec::new();
    for (path, cached_nanos) in cache.iter_resolved() {
        match mtime_nanos(&path) {
            None => stale.push(path),
            Some(c) if c != cached_nanos => stale.push(path),
            _ => {}
        }
    }
    Ok(stale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::{call_edge, include_edge, inherit_edge, make_fg, sym};
    use code_graph_core::{Language, SymbolKind};
    use pretty_assertions::assert_eq;
    use std::fs::OpenOptions;
    use tempfile::TempDir;

    /// Sample graph: two files, mix of edge kinds + an include. Used by
    /// the round-trip and atomic-save tests. Files referenced are not
    /// real on disk; `save` records their mtimes as `0`.
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

    // ---- shape-stable behaviors (v4 → v6 carry-over) ----

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

    #[test]
    fn round_trip_preserves_inherits_edge_kind() {
        // Inherits edges land in adj/radj with EdgeKind::Inherits; verify
        // they survive the encode/decode round-trip.
        let dir = TempDir::new().unwrap();
        let g = build_sample_graph();
        g.save(dir.path()).unwrap();
        let mut loaded = Graph::new();
        assert!(loaded.load(dir.path()).unwrap());
        // `inherit_edge("Derived", "Base", ...)` produces an Inherits
        // edge from bare class name to bare base name — no path
        // prefix — so the adj key is "Derived" and target is "Base".
        let derived_edges = &loaded.adj["Derived"];
        assert!(
            derived_edges.iter().any(
                |e| matches!(e.kind, code_graph_core::EdgeKind::Inherits) && e.target == "Base"
            ),
            "Inherits edge Derived → Base must survive round-trip; got {derived_edges:?}"
        );
    }

    #[test]
    fn save_persists_language_in_files_entry() {
        // FileEntry.language must survive round-trip — was the v4
        // regression that forced cache_v4 (Language was being lost).
        let dir = TempDir::new().unwrap();
        let g = build_sample_graph();
        g.save(dir.path()).unwrap();
        let mut loaded = Graph::new();
        assert!(loaded.load(dir.path()).unwrap());
        for fe in loaded.files.values() {
            assert_eq!(fe.language, Language::Cpp);
        }
    }

    #[test]
    fn load_missing_file_returns_false() {
        let dir = TempDir::new().unwrap();
        let mut g = Graph::new();
        // Pre-populate so we can prove load doesn't touch state on the false path.
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
        // A structurally-valid v6 cache with a wrong version trips the
        // version-check Ok(false) branch — graph state is untouched
        // and the caller is expected to silently re-index.
        let dir = TempDir::new().unwrap();
        let mismatched = serde_json::json!({
            "version": 99,
            "generator": "test",
            "last_sweep_at": 0,
            "paths": [],
            "names": [],
            "nodes": {},
            "adj": {},
            "radj": {},
            "files": {},
            "includes": {},
            "mtimes": {},
        });
        std::fs::write(cache_path(dir.path()), mismatched.to_string()).unwrap();

        let mut g = Graph::new();
        let ok = g.load(dir.path()).unwrap();
        assert!(!ok, "version != CACHE_VERSION → Ok(false)");
    }

    #[test]
    fn load_v4_shape_cache_returns_false() {
        // A leftover v4-shape cache (full strings, no interner tables)
        // fails to deserialize into PackedCacheV6 — the load path
        // converts that into Ok(false) → silent re-index. Confirms the
        // documented v4→v6 cache-invalidation contract.
        let dir = TempDir::new().unwrap();
        let v4_shape = serde_json::json!({
            "version": 4,
            "generator": "code-graph-graph (rust)",
            "nodes": {},
            "adj": {},
            "radj": {},
            "files": {},
            "includes": {},
            "mtimes": {},
        });
        std::fs::write(cache_path(dir.path()), v4_shape.to_string()).unwrap();

        let mut g = Graph::new();
        let ok = g.load(dir.path()).unwrap();
        assert!(
            !ok,
            "v4-shape cache must trip the silent-re-index path (Ok(false)), not error"
        );
    }

    #[test]
    fn load_invalid_json_returns_ok_false() {
        // Pre-v6 behavior was `Err(PersistError::Json)`; v6 silently
        // re-indexes on every parse failure, matching the design
        // (Decision 7 — "Bump only" + cache_load returns Ok(false) on
        // any failure mode short of IO error).
        let dir = TempDir::new().unwrap();
        std::fs::write(cache_path(dir.path()), b"this is not json {[").unwrap();
        let mut g = Graph::new();
        let ok = g.load(dir.path()).unwrap();
        assert!(!ok);
    }

    #[test]
    fn save_overwrites_existing_cache_atomically() {
        let dir = TempDir::new().unwrap();
        // First save.
        let g1 = build_sample_graph();
        g1.save(dir.path()).unwrap();

        // Second save with a different graph — verifies overwrite.
        let mut g2 = Graph::new();
        g2.merge_file_graph(make_fg(
            "/different.cpp",
            Language::Cpp,
            vec![sym("only", SymbolKind::Function, "/different.cpp")],
            vec![],
        ));
        g2.save(dir.path()).unwrap();

        let mut loaded = Graph::new();
        assert!(loaded.load(dir.path()).unwrap());
        assert_eq!(loaded.nodes.len(), 1);
        assert!(loaded.nodes.contains_key("/different.cpp:only"));
        // Tmp file should not survive the rename.
        let tmp = dir.path().join(format!("{CACHE_FILE_NAME}.tmp"));
        assert!(!tmp.exists(), "tmp file must not survive successful save");
    }

    #[test]
    fn save_does_not_disturb_unrelated_tmp_file() {
        // If a previous (crashed) save left a `.tmp` file behind, a
        // subsequent successful save overwrites it cleanly.
        let dir = TempDir::new().unwrap();
        let tmp = dir.path().join(format!("{CACHE_FILE_NAME}.tmp"));
        // Plant a stale tmp.
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)
            .unwrap()
            .write_all(b"stale partial write from a crashed run")
            .unwrap();
        assert!(tmp.exists());

        let g = build_sample_graph();
        g.save(dir.path()).unwrap();

        // Successful save: tmp is consumed by the rename.
        assert!(!tmp.exists(), "save must consume the tmp file via rename");
        // Final cache file exists and is valid.
        let mut loaded = Graph::new();
        assert!(loaded.load(dir.path()).unwrap());
    }

    // ---- stale_paths ----

    #[test]
    fn stale_paths_missing_cache_returns_empty_vec() {
        let dir = TempDir::new().unwrap();
        let stale = stale_paths(dir.path()).unwrap();
        assert!(stale.is_empty());
    }

    #[test]
    fn mtime_invalidation_detects_modified_file() {
        // Save a graph whose `files` references a real on-disk file;
        // touch that file's mtime forward; stale_paths must include it.
        let dir = TempDir::new().unwrap();
        let real_file = dir.path().join("real.cpp");
        std::fs::write(&real_file, b"int main() {}").unwrap();
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            real_file.to_str().unwrap(),
            Language::Cpp,
            vec![sym(
                "main",
                SymbolKind::Function,
                real_file.to_str().unwrap(),
            )],
            vec![],
        ));
        g.save(dir.path()).unwrap();
        // Sleep then re-touch to ensure mtime advances on systems with
        // 1-second mtime resolution.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&real_file, b"int main() { return 0; }").unwrap();

        let stale = stale_paths(dir.path()).unwrap();
        assert!(
            stale.contains(&real_file),
            "modified file must be reported stale; got {:?}",
            stale
        );
    }

    #[test]
    fn mtime_invalidation_detects_deleted_file() {
        let dir = TempDir::new().unwrap();
        let real_file = dir.path().join("realgone.cpp");
        std::fs::write(&real_file, b"//").unwrap();
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            real_file.to_str().unwrap(),
            Language::Cpp,
            vec![sym("x", SymbolKind::Function, real_file.to_str().unwrap())],
            vec![],
        ));
        g.save(dir.path()).unwrap();
        std::fs::remove_file(&real_file).unwrap();

        let stale = stale_paths(dir.path()).unwrap();
        assert!(stale.contains(&real_file));
    }

    // ---- v6-specific guarantees ----

    #[test]
    fn cache_path_returns_correct_filename() {
        let dir = Path::new("/some/dir");
        assert_eq!(
            cache_path(dir),
            PathBuf::from("/some/dir/.code-graph-cache.json")
        );
    }

    #[test]
    fn v6_paths_table_holds_each_path_exactly_once() {
        // Structural check of Phase B's interning claim. With v6, paths
        // appear once in the `paths` table no matter how many `files` /
        // `mtimes` / `Symbol.file` / EdgeEntry.file references touch
        // them. The `names` table separately interns the SymbolId
        // STRINGS (which embed the path) — that residual repetition is
        // an accepted Phase B limitation, eliminated in Phase C when
        // SymbolIds decompose into (PathId, NameId) pairs.
        //
        // We assert on the parsed PackedCacheV6, not raw text, so the
        // test is independent of JSON formatting / SymbolId embedding.
        let dir = TempDir::new().unwrap();
        let file_a = "/very/long/path/a.cpp";
        let file_b = "/very/long/path/b.cpp";

        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            file_a,
            Language::Cpp,
            (0..10)
                .map(|i| sym(&format!("sym_a_{i}"), SymbolKind::Function, file_a))
                .collect(),
            vec![],
        ));
        g.merge_file_graph(make_fg(
            file_b,
            Language::Cpp,
            (0..10)
                .map(|i| sym(&format!("sym_b_{i}"), SymbolKind::Function, file_b))
                .collect(),
            vec![call_edge(
                &format!("{file_b}:sym_b_0"),
                &format!("{file_a}:sym_a_0"),
                file_b,
                1,
            )],
        ));
        g.save(dir.path()).unwrap();

        // Parse back as the raw PackedCacheV6 to inspect interner tables.
        let bytes = std::fs::read(cache_path(dir.path())).unwrap();
        let cache: packed::PackedCacheV6 = serde_json::from_slice(&bytes).unwrap();

        // Each distinct path appears exactly once.
        let path_a_count = cache
            .paths
            .iter()
            .filter(|p| p.as_os_str() == file_a)
            .count();
        let path_b_count = cache
            .paths
            .iter()
            .filter(|p| p.as_os_str() == file_b)
            .count();
        assert_eq!(path_a_count, 1, "file_a must be interned once");
        assert_eq!(path_b_count, 1, "file_b must be interned once");
        assert_eq!(cache.paths.len(), 2, "exactly 2 distinct paths interned");
    }

    #[test]
    fn v6_round_trips_unresolved_edge_target() {
        // Edges to bare-token targets (e.g. `Ok`, `printf` — symbols
        // the parser saw a call to but couldn't resolve to a Symbol
        // record) appear in adj as targets that are NOT in
        // `graph.nodes`. v6 must preserve them through save/load —
        // they live in radj keyed by the bare token, with target =
        // the resolved caller's full SymbolId.
        let dir = TempDir::new().unwrap();
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/main.rs",
            Language::Rust,
            vec![sym("main", SymbolKind::Function, "/main.rs")],
            vec![call_edge("/main.rs:main", "println", "/main.rs", 1)],
        ));
        g.save(dir.path()).unwrap();

        let mut loaded = Graph::new();
        assert!(loaded.load(dir.path()).unwrap());
        // The adj entry for main → println must survive.
        let main_edges = &loaded.adj["/main.rs:main"];
        assert_eq!(main_edges.len(), 1);
        assert_eq!(main_edges[0].target, "println");
        // The radj entry keyed by "println" (the unresolved target)
        // must also survive and point back at main.
        assert!(loaded.radj.contains_key("println"));
        let println_callers = &loaded.radj["println"];
        assert_eq!(println_callers[0].target, "/main.rs:main");
    }
}
