//! Cache v2 persistence for the in-memory [`Graph`].
//!
//! Phase 3.6 of the Rust rewrite. The on-disk cache lives at
//! `<dir>/.code-graph-cache.json`. Two changes versus the Go binary's v1
//! format (`internal/graph/persist.go`):
//!
//! 1. **`FileEntry { language, symbol_ids }`** instead of `[]string`. Phase
//!    2.1 added `Language` to `FileEntry`; v2 records it on disk so [`load`]
//!    does not have to re-derive it from the file extension.
//! 2. **Atomic save**. v1 wrote directly to the final path with
//!    `os.WriteFile`, leaving a partial-write window if the process died
//!    mid-flush. v2 writes to `<dir>/.code-graph-cache.json.tmp`, calls
//!    [`File::sync_all`] to flush data and metadata, then renames over the
//!    final path. The rename is atomic on POSIX and on Windows since Rust
//!    1.84 (which the workspace already requires).
//!
//! Version handling:
//! - v2 cache → loaded.
//! - v1 cache (Go-written) or any other version → silent re-index
//!   (`Ok(false)`). Version mismatch is **not** an error; it is the
//!   expected outcome the first time the Rust binary runs against a cache
//!   produced by the Go binary.
//! - JSON parse errors and IO errors → loud (`Err(PersistError::*)`).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use codegraph_core::{Symbol, SymbolId};
use serde::{Deserialize, Serialize};

use crate::graph::{EdgeEntry, FileEntry, Graph, Node};

const CACHE_FILE_NAME: &str = ".code-graph-cache.json";
const CACHE_VERSION: u32 = 2;
const GENERATOR: &str = "codegraph-graph (rust)";

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
    includes: HashMap<PathBuf, Vec<PathBuf>>,
    /// Nanoseconds since UNIX_EPOCH per indexed file. Stored as `u64`
    /// (Go uses `int64`); truncation is fine because negative timestamps
    /// are not real on any filesystem we support. Files whose mtime cannot
    /// be read are recorded as `0` so they look stale on the next check.
    mtimes: HashMap<PathBuf, u64>,
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

        // Build cache DTO from internal state. `Node` wraps `Symbol`;
        // serialize just the `Symbol` to keep the JSON shape parallel to
        // Go's v1 (where `Nodes` was `map[string]parser.Symbol`).
        let nodes: HashMap<SymbolId, Symbol> = self
            .nodes
            .iter()
            .map(|(id, n)| (id.clone(), n.symbol.clone()))
            .collect();

        // Read mtimes for every indexed file. Files that no longer exist
        // record `0` so a future `stale_paths` call flags them.
        let mut mtimes = HashMap::with_capacity(self.files.len());
        for path in self.files.keys() {
            mtimes.insert(path.clone(), mtime_nanos(path).unwrap_or(0));
        }

        let cache = GraphCache {
            version: CACHE_VERSION,
            generator: GENERATOR.to_string(),
            nodes,
            adj: self.adj.clone(),
            radj: self.radj.clone(),
            files: self.files.clone(),
            includes: self.includes.clone(),
            mtimes,
        };

        // Serialize first so a JSON failure cannot leave a partial tmp file.
        let data = serde_json::to_vec(&cache)?;

        // Write-tmp → sync_all → rename. The braces matter: `sync_all`
        // must run while the `File` is still open.
        {
            let mut f = File::create(&tmp_path)?;
            f.write_all(&data)?;
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
    ///   the cache, re-index. The structurally-valid `version != 2` path
    ///   exists for a future Rust→Rust schema bump.
    pub fn load(&mut self, dir: &Path) -> Result<bool, PersistError> {
        let path = cache_path(dir);
        let data = match fs::read(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(PersistError::Io(e)),
        };

        let cache: GraphCache = serde_json::from_slice(&data)?;
        if cache.version != CACHE_VERSION {
            // v1 (Go-written) or future version: silent re-index. Graph
            // state is left untouched on this branch.
            return Ok(false);
        }

        self.nodes = cache
            .nodes
            .into_iter()
            .map(|(id, symbol)| (id, Node { symbol }))
            .collect();
        self.adj = cache.adj;
        self.radj = cache.radj;
        self.files = cache.files;
        self.includes = cache.includes;
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
    let data = match fs::read(&path) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(PersistError::Io(e)),
    };
    let cache: GraphCache = serde_json::from_slice(&data)?;

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
    use codegraph_core::{EdgeKind, Language, SymbolKind};
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

        // Final file must now be the new content (parses as v2 cache).
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
}
