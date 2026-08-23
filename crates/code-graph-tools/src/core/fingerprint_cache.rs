//! Content-addressed fingerprint cache (FR-37, Designs/VcsHistory
//! Decision 8).
//!
//! A history walk re-reads and re-parses every revision in its window, and
//! the parse dominates; this sidecar makes repeated walks cheap. It lives at
//! `<project_root>/.code-graph/fingerprints/`, entirely separate from
//! `.code-graph-cache.db` — `CACHE_VERSION` is untouched, and a format
//! change here means deleting a directory, not a re-index.
//!
//! **Correctness is structural, not rule-based.** Keys embed the provider's
//! opaque revision token, and revisions identify immutable content in every
//! VCS worth supporting, so a key can never name two different contents and
//! a stale entry cannot be served. Keys also embed a binary identity: the
//! fingerprints themselves come from `DefaultHasher`, which is unstable
//! across Rust releases, so entries written by a different build must read
//! as misses rather than as wrong answers.
//!
//! **The cache is never authoritative and never fails a query.** Every read,
//! decode, or I/O failure is a miss; a corrupt shard is deleted and its
//! entries recomputed; a failed write is ignored. Concurrent writers can
//! lose each other's entries (last write wins per shard) — lost entries are
//! future misses, nothing more.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[cfg(debug_assertions)]
use std::sync::{Arc, Mutex};

use code_graph_lang::FingerprintMode;

/// One cached answer: the symbol's fingerprint at a revision, or a
/// tombstone recording that the symbol was **absent** at that revision.
/// Tombstones are load-bearing: "not present" drives the
/// `Introduced`/`Removed` classification, and without caching it every
/// walk re-parses all the revisions predating the symbol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cached {
    Fingerprint(u64),
    Tombstone,
}

/// The full identity of one cache entry. Everything that could change the
/// answer is part of the key, so lookups need no invalidation rule.
#[derive(Debug)]
pub struct FingerprintKey<'a> {
    /// [`code_graph_vcs::VcsProvider::id`] of the provider that produced
    /// the revision token.
    pub provider: &'a str,
    /// The opaque revision token (display form). Immutable content id.
    pub rev: &'a str,
    /// Path relative to the project root where possible, so the cache
    /// survives a checkout moving on disk.
    pub relative_path: &'a str,
    /// Exact, case-sensitive symbol name (D-0005).
    pub symbol_name: &'a str,
    /// The symbol kind's wire spelling.
    pub kind: &'a str,
    pub mode: FingerprintMode,
    /// Identity of the effective [`code_graph_core::RootConfig`]
    /// (see [`config_identity`]). Extraction is config-dependent — a
    /// `[cpp].macro_*` or `[extensions]` change alters which symbols parse
    /// out of the same revision bytes — so entries written under a
    /// different config must read as misses, exactly like a different
    /// binary.
    pub config: u64,
}

impl FingerprintKey<'_> {
    /// The stored key string. Fields joined with a separator that cannot
    /// appear in any of them (unit separator); the FULL string is stored in
    /// the shard, so distinct keys can never alias — "key mismatch" is
    /// structurally impossible rather than detected.
    fn stored(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{:016x}\u{1f}{:016x}",
            self.provider,
            self.rev,
            self.relative_path,
            self.symbol_name,
            self.kind,
            mode_key(self.mode),
            self.config,
            binary_identity(),
        )
    }
}

/// Identity of the effective config for cache keying. Serialization-based
/// so it needs no `Hash` impl on `RootConfig` and automatically covers
/// every future knob; an unserializable config degrades to a constant,
/// which (like the unreadable-executable arm of [`binary_identity`]) only
/// widens the invalidation blast radius within one config generation.
pub fn config_identity(config: &code_graph_core::RootConfig) -> u64 {
    let Ok(encoded) = serde_json::to_vec(config) else {
        return 0;
    };
    let mut hasher = DefaultHasher::new();
    hasher.write(&encoded);
    hasher.finish()
}

fn mode_key(mode: FingerprintMode) -> &'static str {
    match mode {
        FingerprintMode::Normalized => "normalized",
        FingerprintMode::LiteralInsensitive => "literal_insensitive",
    }
}

/// Identity of the running build. `DefaultHasher` output is only comparable
/// within one compiler's std, so entries are scoped to the executable that
/// wrote them: any rebuild invalidates the cache — conservative and cheap.
/// An unreadable executable degrades to a constant, which keeps the cache
/// functional within a session and merely widens the invalidation blast
/// radius, never the correctness one (fingerprints are still recomputed
/// with the running hasher on a miss).
fn binary_identity() -> u64 {
    static IDENTITY: OnceLock<u64> = OnceLock::new();
    *IDENTITY.get_or_init(|| {
        let Ok(exe) = std::env::current_exe() else {
            return 0;
        };
        let Ok(metadata) = std::fs::metadata(&exe) else {
            return 0;
        };
        let mut hasher = DefaultHasher::new();
        hasher.write(exe.to_string_lossy().as_bytes());
        hasher.write_u64(metadata.len());
        if let Ok(modified) = metadata.modified() {
            if let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) {
                hasher.write_u128(since_epoch.as_nanos());
            }
        }
        hasher.finish()
    })
}

/// The on-disk sidecar. One JSON file per shard, sharded on the key hash's
/// leading byte so a large history does not produce one directory with a
/// hundred thousand entries. Shard value schema: `key -> u64 | null`,
/// where `null` is the tombstone.
#[derive(Clone)]
pub struct FingerprintCache {
    directory: PathBuf,
}

/// Debug-test seam for proving cache shard I/O is off Tokio runtime workers.
/// It is intentionally not enabled in release builds and never affects cache
/// answers.
#[cfg(debug_assertions)]
type GetHook = Arc<dyn Fn() + Send + Sync>;

#[cfg(debug_assertions)]
static GET_HOOK: OnceLock<Mutex<Option<GetHook>>> = OnceLock::new();

#[cfg(debug_assertions)]
#[doc(hidden)]
pub struct GetHookGuard(Option<GetHook>);

#[cfg(debug_assertions)]
impl Drop for GetHookGuard {
    fn drop(&mut self) {
        *GET_HOOK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("fingerprint cache test hook mutex poisoned") = self.0.take();
    }
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn set_get_hook_for_test(hook: GetHook) -> GetHookGuard {
    let previous = (*GET_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("fingerprint cache test hook mutex poisoned"))
    .replace(hook);
    GetHookGuard(previous)
}

type Shard = HashMap<String, Option<u64>>;

impl FingerprintCache {
    /// A cache rooted under `<project_root>/.code-graph/fingerprints`. The
    /// directory is created lazily on the first write.
    pub fn open(project_root: &Path) -> Self {
        Self {
            directory: project_root.join(".code-graph").join("fingerprints"),
        }
    }

    /// Look up one entry. Any failure — missing directory, missing shard,
    /// unreadable or corrupt shard — is a miss; a corrupt shard is deleted
    /// so the next write starts clean.
    pub fn get(&self, key: &FingerprintKey<'_>) -> Option<Cached> {
        #[cfg(debug_assertions)]
        if let Some(hook) = GET_HOOK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("fingerprint cache test hook mutex poisoned")
            .clone()
        {
            hook();
        }
        let stored = key.stored();
        let shard = self.read_shard(&self.shard_path(&stored))?;
        match shard.get(&stored)? {
            Some(fingerprint) => Some(Cached::Fingerprint(*fingerprint)),
            None => Some(Cached::Tombstone),
        }
    }

    /// Record one entry, best-effort: read-modify-write of the shard with
    /// an atomic temp+rename publish. Failures are ignored — the cache is
    /// never authoritative.
    pub fn put(&self, key: &FingerprintKey<'_>, value: Cached) {
        let stored = key.stored();
        let path = self.shard_path(&stored);
        let mut shard = self.read_shard(&path).unwrap_or_default();
        let entry = match value {
            Cached::Fingerprint(fingerprint) => Some(fingerprint),
            Cached::Tombstone => None,
        };
        shard.insert(stored, entry);
        let Ok(encoded) = serde_json::to_vec(&shard) else {
            return;
        };
        if std::fs::create_dir_all(&self.directory).is_err() {
            return;
        }
        // Process id alone is not enough: two walks in one process writing
        // the same shard would share a temp path and could publish a torn
        // file (self-healing, but avoidable for the cost of a counter).
        static WRITE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = WRITE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temp = path.with_extension(format!("tmp-{}-{sequence}", std::process::id()));
        if std::fs::write(&temp, encoded).is_err() {
            return;
        }
        if std::fs::rename(&temp, &path).is_err() {
            let _ = std::fs::remove_file(&temp);
        }
    }

    fn shard_path(&self, stored_key: &str) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        hasher.write(stored_key.as_bytes());
        self.directory
            .join(format!("{:02x}.json", (hasher.finish() >> 56) as u8))
    }

    fn read_shard(&self, path: &Path) -> Option<Shard> {
        let bytes = std::fs::read(path).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(shard) => Some(shard),
            Err(_) => {
                // Corrupt shard: delete and recompute rather than error
                // (AC-46). A failed delete just means another miss later.
                let _ = std::fs::remove_file(path);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn key<'a>(rev: &'a str, name: &'a str, mode: FingerprintMode) -> FingerprintKey<'a> {
        FingerprintKey {
            provider: "git",
            rev,
            relative_path: "src/lib.rs",
            symbol_name: name,
            kind: "function",
            mode,
            config: 0,
        }
    }

    #[test]
    fn fingerprint_cache_hit_after_put() {
        let root = TempDir::new().unwrap();
        let cache = FingerprintCache::open(root.path());
        let k = key("abc123", "target", FingerprintMode::Normalized);
        assert_eq!(cache.get(&k), None, "cold cache misses");
        cache.put(&k, Cached::Fingerprint(42));
        assert_eq!(cache.get(&k), Some(Cached::Fingerprint(42)));
    }

    #[test]
    fn fingerprint_cache_tombstone_roundtrip() {
        let root = TempDir::new().unwrap();
        let cache = FingerprintCache::open(root.path());
        let k = key("abc123", "target", FingerprintMode::Normalized);
        cache.put(&k, Cached::Tombstone);
        assert_eq!(
            cache.get(&k),
            Some(Cached::Tombstone),
            "absence at a revision is a real, reusable answer"
        );
    }

    #[test]
    fn fingerprint_cache_distinct_keys_do_not_alias() {
        let root = TempDir::new().unwrap();
        let cache = FingerprintCache::open(root.path());
        cache.put(
            &key("rev-a", "target", FingerprintMode::Normalized),
            Cached::Fingerprint(1),
        );
        assert_eq!(
            cache.get(&key("rev-b", "target", FingerprintMode::Normalized)),
            None,
            "a different revision is a different key"
        );
        assert_eq!(
            cache.get(&key("rev-a", "other", FingerprintMode::Normalized)),
            None,
            "a different symbol is a different key"
        );
        assert_eq!(
            cache.get(&key("rev-a", "target", FingerprintMode::LiteralInsensitive)),
            None,
            "a different mode is a different key"
        );
        let differently_configured = FingerprintKey {
            config: 1,
            ..key("rev-a", "target", FingerprintMode::Normalized)
        };
        assert_eq!(
            cache.get(&differently_configured),
            None,
            "a different config is a different key — extraction is \
             config-dependent, so entries written under another config must \
             read as misses"
        );
    }

    #[test]
    fn fingerprint_cache_config_identity_tracks_config_content() {
        use code_graph_core::RootConfig;
        let default_config = RootConfig::default();
        let mut stripped = RootConfig::default();
        stripped.cpp.macro_strip.push("CORE_API".to_string());
        assert_eq!(
            config_identity(&default_config),
            config_identity(&RootConfig::default()),
            "identity is deterministic for equal configs"
        );
        assert_ne!(
            config_identity(&default_config),
            config_identity(&stripped),
            "a [cpp].macro_strip change produces a different identity"
        );
    }

    #[test]
    fn fingerprint_cache_deleted_directory_recomputes() {
        let root = TempDir::new().unwrap();
        let cache = FingerprintCache::open(root.path());
        let k = key("abc123", "target", FingerprintMode::Normalized);
        cache.put(&k, Cached::Fingerprint(7));
        std::fs::remove_dir_all(root.path().join(".code-graph/fingerprints")).unwrap();
        assert_eq!(cache.get(&k), None, "a lost cache is a miss, not an error");
        cache.put(&k, Cached::Fingerprint(7));
        assert_eq!(cache.get(&k), Some(Cached::Fingerprint(7)));
    }

    #[test]
    fn fingerprint_cache_corrupt_shard_is_deleted_and_recomputed() {
        let root = TempDir::new().unwrap();
        let cache = FingerprintCache::open(root.path());
        let k = key("abc123", "target", FingerprintMode::Normalized);
        cache.put(&k, Cached::Fingerprint(9));

        // Corrupt every shard file present.
        let directory = root.path().join(".code-graph/fingerprints");
        for entry in std::fs::read_dir(&directory).unwrap() {
            std::fs::write(entry.unwrap().path(), b"{ not json").unwrap();
        }
        assert_eq!(cache.get(&k), None, "corrupt shard reads as a miss");
        assert_eq!(
            std::fs::read_dir(&directory).unwrap().count(),
            0,
            "the corrupt shard was deleted for a clean recompute"
        );
        cache.put(&k, Cached::Fingerprint(9));
        assert_eq!(cache.get(&k), Some(Cached::Fingerprint(9)));
    }

    #[test]
    fn fingerprint_cache_is_independent_of_the_graph_cache() {
        let root = TempDir::new().unwrap();
        let graph_cache = root.path().join(".code-graph-cache.db");
        std::fs::write(&graph_cache, b"graph cache bytes").unwrap();

        let cache = FingerprintCache::open(root.path());
        cache.put(
            &key("abc123", "target", FingerprintMode::Normalized),
            Cached::Fingerprint(3),
        );

        assert_eq!(
            std::fs::read(&graph_cache).unwrap(),
            b"graph cache bytes",
            "the sidecar never touches .code-graph-cache.db"
        );
        assert!(root
            .path()
            .join(".code-graph/fingerprints")
            .read_dir()
            .unwrap()
            .next()
            .is_some());
    }
}
