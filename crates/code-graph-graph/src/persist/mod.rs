//! Versioned cache persistence for the in-memory [`Graph`]. See
//! [`packed::CACHE_VERSION`] for the current version and history.
//!
//! The on-disk cache lives at `<dir>/.code-graph-cache.db` — a rkyv
//! archive prepended by an 8-byte header (endian probe + version).
//! See [`packed`] for the schema and [`mmap`] for the load-time
//! mmap boundary. Saves are atomic: write to `<dir>/.code-graph-cache.db.tmp`,
//! `File::sync_all`, then rename over the final path. The rename is
//! atomic on POSIX and on Windows since Rust 1.84.
//!
//! Version handling:
//! - current-version cache → loaded.
//! - missing file, endian probe mismatch, version mismatch, archive
//!   corruption → silent re-index (`Ok(false)`). None of these are
//!   errors; they are the expected outcome on first run, on cross-
//!   endianness mounts, on cache-version bumps, and on partial-write
//!   recovery.
//! - True IO errors (permission, disk full, etc.) → `Err(PersistError::Io)`.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::graph::Graph;

mod mmap;
pub mod packed;

const CACHE_FILE_NAME: &str = ".code-graph-cache.db";
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
// v6 (interned JSON) → v7 (interned rkyv binary with zero-copy mmap
// load). See `.plans/Designs/PackedCache/README.md`. v7 keeps the
// interned schema from v6 unchanged but switches the on-disk format
// from JSON to a rkyv archive prepended by an 8-byte header (endian
// probe + version). The file extension changes from `.json` to `.db`
// — a separate-inode discriminator that lets the loader treat a
// leftover v6 `.json` cache as "not present" without explicit
// recognition. The schema change is non-backward-compatible; v6
// caches (and any prior) trigger silent re-index per the long-standing
// contract.
//
// v7 → v8 added `confidence` to PackedEdge. See `packed::CACHE_VERSION`
// for the up-to-date doc comment and the per-version history. This
// re-export keeps `CACHE_VERSION` reachable from the original call
// sites without leaving the value defined in two places — a future
// schema change only has to bump `packed::CACHE_VERSION` and the
// header writer / version checks here pick it up automatically.
use packed::CACHE_VERSION;

/// Out-of-scope sweep cadence: 24 hours in nanoseconds. After a scoped
/// `analyze_codebase` finishes its in-scope work, if at least this
/// many nanoseconds have elapsed since the last sweep, the handler
/// runs `Graph::sweep_missing_out_of_scope` to stat every cached file
/// OUTSIDE the invocation scope and drop the ones missing on disk.
/// Keeps the cache from accumulating ghost entries without paying
/// full-revalidation cost on every invocation.
pub const SWEEP_INTERVAL_NANOS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// Errors returned by [`Graph::save`], [`Graph::load`], and [`stale_paths`].
///
/// Version-mismatch, missing-file, endian-probe-mismatch, and archive
/// corruption are **not** errors — they surface as `Ok(false)` on
/// `load` so the caller can silently re-index. Only true IO failures
/// (permission, disk full, etc.) escape as `Err`.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
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
/// the file does not exist, is unreadable, has a pre-epoch mtime, or
/// has a far-future mtime that doesn't fit in `u64` ns (~year 2554).
///
/// The `u128 → u64` boundary matters: `Duration::as_nanos()` returns
/// `u128`, but cache storage is `u64`. A truncating `as u64` cast is
/// silent and stable — if a file's on-disk mtime is past the `u64::MAX`
/// ns horizon (build systems stamp output files to fixed far-future
/// dates, NFS clock skew, etc.), the truncation produces a stable
/// wrong value. The cache writes that truncated value; every later
/// `stale_paths` call reads the same on-disk mtime, re-truncates to
/// the same wrong value, and the equality check matches forever.
/// The file is never reported stale even after edits. Surfacing the
/// overflow as `None` instead routes the file through the
/// "no mtime → treat as stale" path the call sites already handle.
fn mtime_nanos(path: &Path) -> Option<u64> {
    let nanos_u128 = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())?
        .as_nanos();
    u64::try_from(nanos_u128).ok()
}

impl Graph {
    /// Atomically write the graph to `<dir>/.code-graph-cache.db`.
    ///
    /// Strategy: encode, rkyv-serialize, write to a sibling `.tmp`
    /// file with the 8-byte header (endian probe + version) prepended,
    /// `File::sync_all`, then `fs::rename` to swap over the final
    /// path. The rename is atomic on POSIX and on Windows since
    /// Rust 1.84.
    ///
    /// Failure modes:
    /// - Tmp file create / write / rename failure → `Err(PersistError::Io)`.
    /// - rkyv serialization failure → `Err(PersistError::Io)` wrapping
    ///   the underlying error (treated as IO for caller-side
    ///   simplicity; a serialization failure here represents a code
    ///   bug, not a recoverable on-disk state).
    pub fn save(&self, dir: &Path) -> Result<(), PersistError> {
        let final_path = cache_path(dir);
        let tmp_path = dir.join(format!("{CACHE_FILE_NAME}.tmp"));

        // Build the v7 packed cache. The encoder interns paths + name
        // strings into compact tables; mtimes are stat'd fresh inside
        // the encoder (files that no longer exist record `0` so
        // `stale_paths` flags them next round).
        let cache = packed::encode(self, self.last_sweep_at);

        // rkyv-serialize to an AlignedVec. The archive is the
        // post-header byte region; the loader will slice off the
        // first `HEADER_SIZE` bytes before calling `rkyv::access`.
        let archive = rkyv::to_bytes::<rkyv::rancor::Error>(&cache)
            .map_err(|e| io::Error::other(format!("rkyv serialize: {e}")))?;

        // Write header + archive → flush → fsync → rename.
        // Header layout: [ENDIAN_PROBE: u32 native][CACHE_VERSION: u32 native]
        // (see packed::ENDIAN_PROBE / CACHE_VERSION doc-comments).
        {
            let f = File::create(&tmp_path)?;
            let mut writer = io::BufWriter::new(f);
            writer.write_all(&packed::ENDIAN_PROBE.to_ne_bytes())?;
            writer.write_all(&CACHE_VERSION.to_ne_bytes())?;
            writer.write_all(&archive)?;
            writer.flush()?;
            let f = writer
                .into_inner()
                .map_err(|e| io::Error::other(e.into_error()))?;
            f.sync_all()?;
        }
        fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    /// Load the cache from `<dir>/.code-graph-cache.db`.
    ///
    /// Returns:
    /// - `Ok(true)` — cache loaded; graph state replaced.
    /// - `Ok(false)` — cache absent, endian probe mismatch, version
    ///   mismatch, archive corruption, or any other recoverable
    ///   failure short of true IO error. The graph is unchanged; the
    ///   caller should re-index.
    /// - `Err(PersistError::Io)` — read failure other than not-found
    ///   (permission, disk error, etc.).
    pub fn load(&mut self, dir: &Path) -> Result<bool, PersistError> {
        let path = cache_path(dir);

        // mmap the cache file (read-only, zero-copy). The one unsafe
        // boundary in this crate; see `mmap::mmap_read_only`'s SAFETY
        // block.
        let holder = match mmap::mmap_read_only(&path)? {
            Some(h) => h,
            None => return Ok(false), // cache file absent or zero-byte
        };
        let bytes = holder.as_bytes();

        // Header: too small → treat as cache-absent.
        if bytes.len() < packed::HEADER_SIZE {
            return Ok(false);
        }

        // Endian probe: a host whose native u32 doesn't match
        // ENDIAN_PROBE was either written on a different endianness
        // (rare) or read a corrupted file. Re-index either way.
        let probe = u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if probe != packed::ENDIAN_PROBE {
            return Ok(false);
        }
        let version = u32::from_ne_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        if version != CACHE_VERSION {
            return Ok(false);
        }

        // rkyv::access validates the archive (bytecheck pass) and
        // returns a typed view into the mapped bytes. Bytecheck
        // failure → corrupt cache → re-index. We then walk the
        // archived view directly via `packed::decode_archived`,
        // skipping the intermediate `rkyv::deserialize` pass that
        // would have allocated a full owned `PackedCacheV6` on the
        // heap. The direct walk allocates only the live Graph's
        // HashMaps, saving one O(N) allocation pass on every load.
        let archive_bytes = &bytes[packed::HEADER_SIZE..];
        let archived = match rkyv::access::<
            <packed::PackedCacheV6 as rkyv::Archive>::Archived,
            rkyv::rancor::Error,
        >(archive_bytes)
        {
            Ok(a) => a,
            Err(_) => return Ok(false),
        };

        // Sanity-check the embedded version field (the rkyv version
        // and the header version should agree; if they don't, treat
        // as corrupt).
        if archived.version.to_native() != CACHE_VERSION {
            return Ok(false);
        }

        let parts = match packed::decode_archived(archived) {
            Ok(p) => p,
            Err(_) => return Ok(false),
        };

        self.nodes = parts.nodes;
        self.adj = parts.adj;
        self.radj = parts.radj;
        self.files = parts.files;
        self.includes = parts.includes;
        self.last_sweep_at = parts.last_sweep_at;
        Ok(true)
    }
}

/// Returns indexed files whose on-disk mtime differs from the cached
/// mtime. Files that no longer exist (or whose mtime cannot be read) are
/// included so the indexer treats them as stale and re-walks them.
///
/// **v7 strategy** (PackedCache design Decision 6): mmap the cache,
/// validate the header, run rkyv's bytecheck, then read just the
/// `paths` and `mtimes` fields off the resulting archived view. The
/// bytecheck cost on a ~25 MB cache is roughly a memcpy-speed scan
/// (10-50 ms), 50-150× cheaper than the v4 slim-DTO-over-200MB-JSON
/// trick this replaces. If bench numbers ever push past ~200 ms on a
/// million-symbol cache, the design's Option 1 fallback — a sidecar
/// `.code-graph-cache-mtimes.db` file — remains shelved-and-ready.
///
/// **Missing / unreadable cache:** returns `Ok(vec![])`. Matches
/// `Graph::load`'s `Ok(false)` ergonomics — callers can speculatively
/// invoke `stale_paths` before `load` without a special-case for
/// first-run.
pub fn stale_paths(dir: &Path) -> Result<Vec<PathBuf>, PersistError> {
    let path = cache_path(dir);

    let holder = match mmap::mmap_read_only(&path)? {
        Some(h) => h,
        None => return Ok(Vec::new()),
    };
    let bytes = holder.as_bytes();
    if bytes.len() < packed::HEADER_SIZE {
        return Ok(Vec::new());
    }
    let probe = u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if probe != packed::ENDIAN_PROBE {
        return Ok(Vec::new());
    }
    let version = u32::from_ne_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if version != CACHE_VERSION {
        return Ok(Vec::new());
    }

    let archive_bytes = &bytes[packed::HEADER_SIZE..];
    let archived = match rkyv::access::<
        <packed::PackedCacheV6 as rkyv::Archive>::Archived,
        rkyv::rancor::Error,
    >(archive_bytes)
    {
        Ok(a) => a,
        Err(_) => return Ok(Vec::new()),
    };

    // Walk the archived view directly for `paths` + `mtimes` — no
    // owned-`PackedCacheV6` allocation needed. `ArchivedHashMap`
    // iteration yields `(&u32, &u64)` pairs; we resolve each PathId
    // against the `ArchivedVec<ArchivedString>` paths table.
    let mut stale = Vec::new();
    for (id, cached_nanos) in archived.mtimes.iter() {
        let id_val: u32 = (*id).into();
        if id_val == 0 {
            continue;
        }
        let idx = id_val as usize - 1;
        let Some(archived_path) = archived.paths.get(idx) else {
            continue;
        };
        let path = PathBuf::from(archived_path.as_str());
        let cached_val: u64 = (*cached_nanos).into();
        match mtime_nanos(&path) {
            None => stale.push(path),
            Some(c) if c != cached_val => stale.push(path),
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
            PathBuf::from("/some/dir/.code-graph-cache.db")
        );
    }

    #[test]
    fn v7_paths_table_holds_each_path_exactly_once() {
        // Structural check of the interning claim. Each distinct path
        // appears once in the `paths` table no matter how many
        // `files` / `mtimes` / `Symbol.file` / EdgeEntry.file
        // references touch it. (The `names` table separately interns
        // the SymbolId STRINGS — which embed the path — and that
        // residual repetition is an accepted limitation eliminated
        // only if/when SymbolIds decompose into (PathId, NameId) in a
        // future phase.)
        //
        // We round-trip through `Graph::save` + a fresh `Graph::load`
        // and inspect the loaded `Graph.files` (which mirrors paths
        // 1-1 after decode). On a v7 rkyv cache we can't `jq` the
        // file directly; verifying via `paths.len()` proxies for the
        // structural property because the encoder fails fast if the
        // interner has duplicates.
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

        // The live graph has exactly 2 file entries — one per
        // distinct path. If interning duplicated a path, the loaded
        // graph would have more files because the encoder would have
        // assigned different PathIds and the decoder would have
        // produced separate FileEntry rows. Use this as the
        // observable proxy for "each path interned once."
        let mut loaded = Graph::new();
        assert!(loaded.load(dir.path()).unwrap());
        assert_eq!(loaded.files.len(), 2);
        assert!(loaded.files.contains_path(PathBuf::from(file_a)));
        assert!(loaded.files.contains_path(PathBuf::from(file_b)));
        // Stronger: each FileEntry has the right symbol count.
        assert_eq!(
            loaded
                .files
                .get(PathBuf::from(file_a))
                .unwrap()
                .symbol_ids
                .len(),
            10
        );
        assert_eq!(
            loaded
                .files
                .get(PathBuf::from(file_b))
                .unwrap()
                .symbol_ids
                .len(),
            10
        );
    }

    #[test]
    fn v7_endian_probe_mismatch_returns_ok_false() {
        // A cache file whose first 4 bytes don't form `ENDIAN_PROBE`
        // when read native-endian — either because it was written by
        // a different-endianness host, or because it's not a v7 cache
        // at all (e.g. random bytes) — must silently re-index.
        let dir = TempDir::new().unwrap();
        // Plant a file whose first 4 bytes are deliberately wrong.
        let mut bytes = vec![0xDE, 0xAD, 0xBE, 0xEF];
        bytes.extend_from_slice(&CACHE_VERSION.to_ne_bytes());
        bytes.extend_from_slice(
            b"junk archive bytes that would fail bytecheck even if header passed",
        );
        std::fs::write(cache_path(dir.path()), &bytes).unwrap();

        let mut g = Graph::new();
        let ok = g.load(dir.path()).unwrap();
        assert!(
            !ok,
            "endian probe mismatch must trigger silent re-index path"
        );
    }

    #[test]
    fn v7_truncated_archive_returns_ok_false() {
        // Save a valid v7 cache, then truncate it mid-archive. Load
        // must report Ok(false) (bytecheck fails on truncated data).
        let dir = TempDir::new().unwrap();
        let g = build_sample_graph();
        g.save(dir.path()).unwrap();

        let path = cache_path(dir.path());
        let original = std::fs::read(&path).unwrap();
        // Keep header + first 16 bytes of archive — definitely
        // truncated mid-structure.
        let truncated_len = packed::HEADER_SIZE + 16;
        assert!(
            original.len() > truncated_len,
            "test precondition: full v7 cache is more than {truncated_len} bytes"
        );
        std::fs::write(&path, &original[..truncated_len]).unwrap();

        let mut loaded = Graph::new();
        let ok = loaded.load(dir.path()).unwrap();
        assert!(!ok, "truncated v7 archive must trigger silent re-index");
    }

    #[test]
    fn v7_header_only_file_returns_ok_false() {
        // A file that contains ONLY the 8-byte header (probe + version)
        // and no archive body has nothing to bytecheck. Must Ok(false).
        let dir = TempDir::new().unwrap();
        let mut bytes = packed::ENDIAN_PROBE.to_ne_bytes().to_vec();
        bytes.extend_from_slice(&CACHE_VERSION.to_ne_bytes());
        std::fs::write(cache_path(dir.path()), &bytes).unwrap();

        let mut g = Graph::new();
        assert!(!g.load(dir.path()).unwrap());
    }

    #[test]
    fn v7_round_trips_unresolved_edge_target() {
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
