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
//! entry recomputed; a failed write is ignored. Concurrent eviction can turn
//! a hit into a miss, never into a wrong answer.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(debug_assertions)]
use std::sync::Mutex;

use code_graph_lang::FingerprintMode;
use serde::{Deserialize, Serialize};

const KEY_FORMAT_VERSION: u8 = 2;
const SHARD_FORMAT_VERSION: u8 = 2;
const SHARD_DIRECTORY: &str = "v2";
const MAINTENANCE_LOCK: &str = ".maintenance.lock";
const HIGH_WATER_BYTES: u64 = 256 * 1024 * 1024;
const LOW_WATER_BYTES: u64 = 192 * 1024 * 1024;
const HIGH_WATER_SHARDS: usize = 100_000;
const LOW_WATER_SHARDS: usize = 75_000;
const MAINTENANCE_WRITE_INTERVAL: u64 = 256;

/// One cached answer: the symbol's fingerprint at a revision, or a
/// tombstone recording that the symbol was **absent** at that revision.
/// Tombstones are load-bearing: "not present" drives the
/// `Introduced`/`Removed` classification, and without caching it every
/// walk re-parses all the revisions predating the symbol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cached {
    Fingerprint(u64),
    Tombstone,
    /// Versioned negative outcome for a span that the selected AST mode
    /// cannot fingerprint. This is uncertainty, never symbol absence.
    UnfingerprintableV1,
}

/// One owned cache key. Its encoded form is length-framed before hashing, and
/// the full encoding is stored in the shard so a filename-hash collision can
/// only cause a miss, never a wrong answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FingerprintKey {
    stored: String,
    relative_shard: PathBuf,
}

impl FingerprintKey {
    pub fn relative_shard(&self) -> &Path {
        &self.relative_shard
    }
}

/// Central builder for every history cache key. A missing executable identity
/// disables cache reads and writes for the walk instead of making two unknown
/// binaries alias under a shared sentinel.
#[derive(Clone, Debug)]
pub struct FingerprintKeyBuilder {
    provider: String,
    relative_path: String,
    symbol_name: String,
    kind: String,
    language: code_graph_core::Language,
    mode: FingerprintMode,
    config: u64,
    binary: Option<u64>,
}

impl FingerprintKeyBuilder {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: String,
        relative_path: String,
        symbol_name: String,
        kind: String,
        language: code_graph_core::Language,
        mode: FingerprintMode,
        config: &code_graph_core::RootConfig,
    ) -> Self {
        Self {
            provider,
            relative_path,
            symbol_name,
            kind,
            language,
            mode,
            config: config_identity(config),
            binary: binary_identity(),
        }
    }

    #[cfg(test)]
    fn with_binary_identity(mut self, binary: Option<u64>) -> Self {
        self.binary = binary;
        self
    }

    pub fn build(&self, rev: &str) -> Option<FingerprintKey> {
        let binary = self.binary?;
        let mut framed = vec![KEY_FORMAT_VERSION];
        let language = format!("{:?}", self.language);
        let config = self.config.to_le_bytes();
        let binary = binary.to_le_bytes();
        for field in [
            self.provider.as_bytes(),
            rev.as_bytes(),
            self.relative_path.as_bytes(),
            self.symbol_name.as_bytes(),
            self.kind.as_bytes(),
            language.as_bytes(),
            mode_key(self.mode).as_bytes(),
            &config,
            &binary,
        ] {
            write_field(&mut framed, field);
        }
        let stored = hex(&framed);
        let mut hasher = DefaultHasher::new();
        hasher.write(&framed);
        let hash = format!("{:016x}", hasher.finish());
        let relative_shard = PathBuf::from(SHARD_DIRECTORY)
            .join(&hash[..2])
            .join(format!("{hash}.json"));
        Some(FingerprintKey {
            stored,
            relative_shard,
        })
    }
}

/// Identity of extraction-relevant configuration only. Runtime response,
/// daemon, discovery, and thread-pool knobs cannot alter historical parsing;
/// C++ preprocessing/synthesis and extension dispatch can.
pub fn config_identity(config: &code_graph_core::RootConfig) -> u64 {
    let Ok(encoded) = serde_json::to_vec(&(&config.cpp, &config.extensions)) else {
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

fn write_field(output: &mut Vec<u8>, field: &[u8]) {
    output.extend_from_slice(&(field.len() as u64).to_le_bytes());
    output.extend_from_slice(field);
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// Content identity of the running executable. If the executable cannot be
/// read, caching is disabled for the walk: no fallback value can honestly
/// claim that two unreadable binaries use the same `DefaultHasher` semantics.
fn binary_identity() -> Option<u64> {
    static IDENTITY: OnceLock<Option<u64>> = OnceLock::new();
    *IDENTITY.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        let bytes = std::fs::read(exe).ok()?;
        let mut hasher = DefaultHasher::new();
        hasher.write_u64(bytes.len() as u64);
        hasher.write(&bytes);
        Some(hasher.finish())
    })
}

#[derive(Clone, Copy, Debug)]
struct MaintenanceLimits {
    high_bytes: u64,
    low_bytes: u64,
    high_shards: usize,
    low_shards: usize,
    write_interval: u64,
}

const DEFAULT_LIMITS: MaintenanceLimits = MaintenanceLimits {
    high_bytes: HIGH_WATER_BYTES,
    low_bytes: LOW_WATER_BYTES,
    high_shards: HIGH_WATER_SHARDS,
    low_shards: LOW_WATER_SHARDS,
    write_interval: MAINTENANCE_WRITE_INTERVAL,
};

/// The on-disk sidecar. Version 2 stores one completed shard per key under a
/// hash-prefix directory. The full framed key remains inside each shard, so
/// hash collisions degrade to misses. Old root-level v1 shards are ignored by
/// reads but remain eligible for lifecycle trimming.
#[derive(Clone)]
pub struct FingerprintCache {
    directory: PathBuf,
    protected_shards: Arc<HashSet<PathBuf>>,
    successful_writes: Arc<AtomicU64>,
    limits: MaintenanceLimits,
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

#[derive(Debug, Deserialize, Serialize)]
struct StoredShard {
    version: u8,
    key: String,
    outcome: StoredOutcome,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
enum StoredOutcome {
    Fingerprint(u64),
    Tombstone,
    UnfingerprintableV1,
}

impl From<Cached> for StoredOutcome {
    fn from(value: Cached) -> Self {
        match value {
            Cached::Fingerprint(fingerprint) => Self::Fingerprint(fingerprint),
            Cached::Tombstone => Self::Tombstone,
            Cached::UnfingerprintableV1 => Self::UnfingerprintableV1,
        }
    }
}

impl From<&StoredOutcome> for Cached {
    fn from(value: &StoredOutcome) -> Self {
        match value {
            StoredOutcome::Fingerprint(fingerprint) => Self::Fingerprint(*fingerprint),
            StoredOutcome::Tombstone => Self::Tombstone,
            StoredOutcome::UnfingerprintableV1 => Self::UnfingerprintableV1,
        }
    }
}

#[derive(Debug)]
struct CompletedShard {
    path: PathBuf,
    relative: PathBuf,
    bytes: u64,
    modified: SystemTime,
}

impl FingerprintCache {
    /// Open the sidecar and perform one best-effort maintenance pass. Callers
    /// pass the current walk's shard paths so its own entries are not pruned.
    /// The directory is still created lazily on the first write.
    pub fn open(project_root: &Path, protected_shards: HashSet<PathBuf>) -> Self {
        let cache = Self {
            directory: project_root.join(".code-graph").join("fingerprints"),
            protected_shards: Arc::new(protected_shards),
            successful_writes: Arc::new(AtomicU64::new(0)),
            limits: DEFAULT_LIMITS,
        };
        cache.maintain();
        cache
    }

    /// Look up one entry. Any failure — missing directory, missing shard,
    /// unreadable or corrupt shard — is a miss; a corrupt shard is deleted
    /// so the next write starts clean.
    pub fn get(&self, key: &FingerprintKey) -> Option<Cached> {
        #[cfg(debug_assertions)]
        if let Some(hook) = GET_HOOK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("fingerprint cache test hook mutex poisoned")
            .clone()
        {
            hook();
        }
        let path = self.directory.join(&key.relative_shard);
        let shard = self.read_shard(&path)?;
        if shard.version != SHARD_FORMAT_VERSION || shard.key != key.stored {
            let _ = std::fs::remove_file(path);
            return None;
        }
        Some(Cached::from(&shard.outcome))
    }

    /// Record one entry with an atomic temp+rename publish. Failures are
    /// ignored — the cache is never authoritative.
    pub fn put(&self, key: &FingerprintKey, value: Cached) {
        let path = self.directory.join(&key.relative_shard);
        let shard = StoredShard {
            version: SHARD_FORMAT_VERSION,
            key: key.stored.clone(),
            outcome: value.into(),
        };
        let Ok(encoded) = serde_json::to_vec(&shard) else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
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
            return;
        }
        let writes = self.successful_writes.fetch_add(1, Ordering::Relaxed) + 1;
        if writes.is_multiple_of(self.limits.write_interval) {
            self.maintain();
        }
    }

    fn read_shard(&self, path: &Path) -> Option<StoredShard> {
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

    fn maintain(&self) {
        if !is_real_directory(&self.directory) {
            return;
        }
        let lock_path = self.directory.join(MAINTENANCE_LOCK);
        let Ok(lock) = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
        else {
            return;
        };
        if lock.try_lock().is_err() {
            return;
        }
        self.maintain_locked();
        let _ = lock.unlock();
    }

    fn maintain_locked(&self) {
        let mut shards = Vec::new();
        collect_completed_shards(&self.directory, &mut shards);
        let mut total_bytes = shards
            .iter()
            .fold(0_u64, |total, shard| total.saturating_add(shard.bytes));
        let mut total_shards = shards.len();
        if total_bytes <= self.limits.high_bytes && total_shards <= self.limits.high_shards {
            return;
        }

        shards.sort_by(|left, right| {
            left.modified
                .cmp(&right.modified)
                .then_with(|| left.relative.cmp(&right.relative))
        });
        for shard in shards {
            if total_bytes <= self.limits.low_bytes && total_shards <= self.limits.low_shards {
                break;
            }
            if self.protected_shards.contains(&shard.relative) {
                continue;
            }
            if std::fs::remove_file(&shard.path).is_ok() {
                total_bytes = total_bytes.saturating_sub(shard.bytes);
                total_shards = total_shards.saturating_sub(1);
            }
        }

        if total_bytes > self.limits.low_bytes || total_shards > self.limits.low_shards {
            eprintln!(
                "fingerprint cache maintenance stopped above low-water marks: \
                 {total_bytes} bytes in {total_shards} completed shards remain"
            );
        }
    }
}

/// Enumerate only the two recognized completed-shard layouts: legacy v1
/// `??.json` files at the sidecar root and v2 `??/<hash>.json` files. A
/// bounded two-level scan avoids following arbitrary directory trees, and
/// every traversed directory/file must be a real entry rather than a symlink.
fn collect_completed_shards(root: &Path, output: &mut Vec<CompletedShard>) {
    collect_shard_files(root, root, is_legacy_shard_name, output);
    let v2 = root.join(SHARD_DIRECTORY);
    if !is_real_directory(&v2) {
        return;
    }
    let Ok(prefixes) = std::fs::read_dir(&v2) else {
        return;
    };
    for prefix in prefixes.flatten() {
        let path = prefix.path();
        if !is_hex_name(&prefix.file_name(), 2) || !is_real_directory(&path) {
            continue;
        }
        collect_shard_files(root, &path, is_v2_shard_name, output);
    }
}

fn collect_shard_files(
    root: &Path,
    directory: &Path,
    accepts_name: fn(&std::ffi::OsStr) -> bool,
    output: &mut Vec<CompletedShard>,
) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() || !accepts_name(&entry.file_name()) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(root).map(Path::to_path_buf) else {
            continue;
        };
        output.push(CompletedShard {
            path,
            relative,
            bytes: metadata.len(),
            modified: metadata.modified().unwrap_or(UNIX_EPOCH),
        });
    }
}

fn is_real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_dir())
        .unwrap_or(false)
}

fn is_legacy_shard_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.len() == 7
        && name.ends_with(".json")
        && name.as_bytes()[..2].iter().all(u8::is_ascii_hexdigit)
}

fn is_v2_shard_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.len() == 21
        && name.ends_with(".json")
        && name.as_bytes()[..16].iter().all(u8::is_ascii_hexdigit)
}

fn is_hex_name(name: &std::ffi::OsStr, length: usize) -> bool {
    name.to_str().is_some_and(|name| {
        name.len() == length && name.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph_core::{Language, RootConfig};
    use std::process::Command;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    fn builder(name: &str, mode: FingerprintMode) -> FingerprintKeyBuilder {
        FingerprintKeyBuilder::new(
            "git".to_string(),
            "src/lib.rs".to_string(),
            name.to_string(),
            "function".to_string(),
            Language::Rust,
            mode,
            &RootConfig::default(),
        )
        .with_binary_identity(Some(7))
    }

    fn key(rev: &str, name: &str, mode: FingerprintMode) -> FingerprintKey {
        builder(name, mode).build(rev).unwrap()
    }

    fn cache(root: &TempDir) -> FingerprintCache {
        FingerprintCache::open(root.path(), HashSet::new())
    }

    fn cache_with_limits(
        root: &TempDir,
        protected_shards: HashSet<PathBuf>,
        limits: MaintenanceLimits,
    ) -> FingerprintCache {
        let cache = FingerprintCache {
            directory: root.path().join(".code-graph").join("fingerprints"),
            protected_shards: Arc::new(protected_shards),
            successful_writes: Arc::new(AtomicU64::new(0)),
            limits,
        };
        cache.maintain();
        cache
    }

    fn completed_shards(cache: &FingerprintCache) -> Vec<CompletedShard> {
        let mut shards = Vec::new();
        collect_completed_shards(&cache.directory, &mut shards);
        shards
    }

    fn write_completed(cache: &FingerprintCache, relative: &str, bytes: usize) -> PathBuf {
        let path = cache.directory.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![b'x'; bytes]).unwrap();
        path
    }

    fn wait_for_child(child: &mut std::process::Child) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("timed out waiting for maintenance child");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn fingerprint_cache_hit_after_put() {
        let root = TempDir::new().unwrap();
        let cache = cache(&root);
        let k = key("abc123", "target", FingerprintMode::Normalized);
        assert_eq!(cache.get(&k), None, "cold cache misses");
        cache.put(&k, Cached::Fingerprint(42));
        assert_eq!(cache.get(&k), Some(Cached::Fingerprint(42)));
    }

    #[test]
    fn fingerprint_cache_tombstone_roundtrip() {
        let root = TempDir::new().unwrap();
        let cache = cache(&root);
        let k = key("abc123", "target", FingerprintMode::Normalized);
        cache.put(&k, Cached::Tombstone);
        assert_eq!(
            cache.get(&k),
            Some(Cached::Tombstone),
            "absence at a revision is a real, reusable answer"
        );
    }

    #[test]
    fn fingerprint_cache_unfingerprintable_outcome_roundtrip() {
        let root = TempDir::new().unwrap();
        let cache = cache(&root);
        let k = key("abc123", "target", FingerprintMode::LiteralInsensitive);
        cache.put(&k, Cached::UnfingerprintableV1);
        assert_eq!(cache.get(&k), Some(Cached::UnfingerprintableV1));
    }

    #[test]
    fn fingerprint_cache_distinct_keys_do_not_alias() {
        let root = TempDir::new().unwrap();
        let cache = cache(&root);
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
        let mut changed = RootConfig::default();
        changed.cpp.macro_strip.push("CORE_API".to_string());
        let differently_configured = FingerprintKeyBuilder::new(
            "git".to_string(),
            "src/lib.rs".to_string(),
            "target".to_string(),
            "function".to_string(),
            Language::Rust,
            FingerprintMode::Normalized,
            &changed,
        )
        .with_binary_identity(Some(7))
        .build("rev-a")
        .unwrap();
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
        let default_config = RootConfig::default();
        let mut stripped = RootConfig::default();
        stripped.cpp.macro_strip.push("CORE_API".to_string());
        let mut extensions = RootConfig::default();
        extensions.extensions.rust.push(".rust".to_string());
        let mut runtime_only = RootConfig::default();
        runtime_only.response.max_bytes += 1;
        runtime_only.daemon.enabled = false;
        runtime_only.discovery.max_threads = 3;
        runtime_only.parsing.max_threads = 2;
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
        assert_ne!(
            config_identity(&default_config),
            config_identity(&extensions),
            "extension dispatch participates in extraction identity"
        );
        assert_eq!(
            config_identity(&default_config),
            config_identity(&runtime_only),
            "runtime, response, discovery, and concurrency settings do not invalidate fingerprints"
        );
    }

    #[test]
    fn framed_keys_do_not_alias_on_raw_unit_separator() {
        let config = RootConfig::default();
        let left = FingerprintKeyBuilder::new(
            "git\u{1f}revision".to_string(),
            "src/lib.rs".to_string(),
            "target".to_string(),
            "function".to_string(),
            Language::Rust,
            FingerprintMode::Normalized,
            &config,
        )
        .with_binary_identity(Some(7))
        .build("value")
        .unwrap();
        let right = FingerprintKeyBuilder::new(
            "git".to_string(),
            "src/lib.rs".to_string(),
            "target".to_string(),
            "function".to_string(),
            Language::Rust,
            FingerprintMode::Normalized,
            &config,
        )
        .with_binary_identity(Some(7))
        .build("revision\u{1f}value")
        .unwrap();
        assert_ne!(left.stored, right.stored);
        assert_ne!(left.relative_shard, right.relative_shard);
    }

    #[test]
    fn unavailable_binary_identity_disables_cache_keying() {
        assert!(builder("target", FingerprintMode::Normalized)
            .with_binary_identity(None)
            .build("abc123")
            .is_none());
    }

    #[test]
    fn fingerprint_cache_deleted_directory_recomputes() {
        let root = TempDir::new().unwrap();
        let cache = cache(&root);
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
        let cache = cache(&root);
        let k = key("abc123", "target", FingerprintMode::Normalized);
        cache.put(&k, Cached::Fingerprint(9));

        let path = cache.directory.join(k.relative_shard());
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(cache.get(&k), None, "corrupt shard reads as a miss");
        assert!(!path.exists(), "the corrupt shard was deleted");
        cache.put(&k, Cached::Fingerprint(9));
        assert_eq!(cache.get(&k), Some(Cached::Fingerprint(9)));
    }

    #[test]
    fn fingerprint_cache_is_independent_of_the_graph_cache() {
        let root = TempDir::new().unwrap();
        let graph_cache = root.path().join(".code-graph-cache.db");
        std::fs::write(&graph_cache, b"graph cache bytes").unwrap();

        let cache = cache(&root);
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

    #[test]
    fn maintenance_trims_by_bytes_and_preserves_temp_and_protected_shards() {
        let root = TempDir::new().unwrap();
        let bootstrap = cache(&root);
        let protected = PathBuf::from("v2/aa/0000000000000001.json");
        write_completed(&bootstrap, "v2/aa/0000000000000001.json", 10);
        write_completed(&bootstrap, "v2/aa/0000000000000002.json", 10);
        write_completed(&bootstrap, "v2/aa/0000000000000003.json", 10);
        let temp = bootstrap.directory.join("v2/aa/write.tmp-1-1");
        std::fs::write(&temp, b"temporary").unwrap();

        let cache = cache_with_limits(
            &root,
            HashSet::from([protected.clone()]),
            MaintenanceLimits {
                high_bytes: 25,
                low_bytes: 10,
                high_shards: usize::MAX,
                low_shards: usize::MAX,
                write_interval: 256,
            },
        );
        let shards = completed_shards(&cache);
        assert_eq!(shards.len(), 1);
        assert_eq!(shards[0].relative, protected);
        assert!(temp.exists(), "maintenance never deletes temp files");
    }

    #[test]
    fn maintenance_trims_by_count_on_open_and_after_write_interval() {
        let root = TempDir::new().unwrap();
        let bootstrap = cache(&root);
        write_completed(&bootstrap, "v2/aa/0000000000000001.json", 1);
        write_completed(&bootstrap, "v2/aa/0000000000000002.json", 1);
        write_completed(&bootstrap, "v2/aa/0000000000000003.json", 1);
        let cache = cache_with_limits(
            &root,
            HashSet::new(),
            MaintenanceLimits {
                high_bytes: u64::MAX,
                low_bytes: u64::MAX,
                high_shards: 2,
                low_shards: 1,
                write_interval: 2,
            },
        );
        assert_eq!(completed_shards(&cache).len(), 1, "open triggers trimming");

        cache.put(
            &key("write-1", "target", FingerprintMode::Normalized),
            Cached::Fingerprint(1),
        );
        cache.put(
            &key("write-2", "target", FingerprintMode::Normalized),
            Cached::Fingerprint(2),
        );
        assert_eq!(
            completed_shards(&cache).len(),
            1,
            "the configured successful-write interval triggers trimming"
        );
    }

    #[test]
    fn concurrent_eviction_is_a_harmless_cache_miss() {
        let root = TempDir::new().unwrap();
        let cache = cache(&root);
        let k = key("abc123", "target", FingerprintMode::Normalized);
        cache.put(&k, Cached::Fingerprint(7));
        std::fs::remove_file(cache.directory.join(k.relative_shard())).unwrap();
        assert_eq!(cache.get(&k), None);
        cache.put(&k, Cached::Fingerprint(7));
        assert_eq!(cache.get(&k), Some(Cached::Fingerprint(7)));
    }

    #[test]
    fn maintenance_lock_is_shared_between_independent_handles() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join(".code-graph/fingerprints");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(MAINTENANCE_LOCK);
        let first = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        first.lock().unwrap();
        assert!(
            second.try_lock().is_err(),
            "an independent process handle cannot enter maintenance concurrently"
        );
        first.unlock().unwrap();
        second.try_lock().unwrap();
        second.unlock().unwrap();
    }

    #[test]
    fn maintenance_lock_contention_is_skipped_across_processes() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join(".code-graph/fingerprints");
        std::fs::create_dir_all(&directory).unwrap();
        let lock_path = directory.join(MAINTENANCE_LOCK);
        let completed = root.path().join("child-completed");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        lock.lock().unwrap();

        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "core::fingerprint_cache::tests::maintenance_lock_child",
                "--nocapture",
            ])
            .env("CODE_GRAPH_MAINTENANCE_LOCK_CHILD", "1")
            .env("CODE_GRAPH_MAINTENANCE_ROOT", root.path())
            .env("CODE_GRAPH_MAINTENANCE_COMPLETED", &completed)
            .spawn()
            .unwrap();
        let status = wait_for_child(&mut child);
        assert!(
            status.success(),
            "cache open skips maintenance rather than waiting for the process-shared lock"
        );
        assert!(completed.exists());
        lock.unlock().unwrap();
    }

    #[test]
    fn maintenance_lock_child() {
        if std::env::var_os("CODE_GRAPH_MAINTENANCE_LOCK_CHILD").is_none() {
            return;
        }
        let root = PathBuf::from(std::env::var_os("CODE_GRAPH_MAINTENANCE_ROOT").unwrap());
        let completed =
            PathBuf::from(std::env::var_os("CODE_GRAPH_MAINTENANCE_COMPLETED").unwrap());
        let _cache = FingerprintCache::open(&root, HashSet::new());
        std::fs::write(completed, b"completed").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn maintenance_does_not_follow_symlinked_shard_directories() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let bootstrap = cache(&root);
        let v2 = bootstrap.directory.join(SHARD_DIRECTORY);
        std::fs::create_dir_all(&v2).unwrap();
        let outside_shard = outside.path().join("0000000000000001.json");
        std::fs::write(&outside_shard, b"outside").unwrap();
        symlink(outside.path(), v2.join("aa")).unwrap();

        let _cache = cache_with_limits(
            &root,
            HashSet::new(),
            MaintenanceLimits {
                high_bytes: 0,
                low_bytes: 0,
                high_shards: 0,
                low_shards: 0,
                write_interval: 1,
            },
        );

        assert!(outside_shard.exists(), "maintenance never follows symlinks");
    }

    #[test]
    fn cross_process_publish_and_eviction_are_atomic_or_misses() {
        let root = TempDir::new().unwrap();
        let cache = cache(&root);
        let k = key("abc123", "target", FingerprintMode::Normalized);
        cache.put(&k, Cached::Fingerprint(7));

        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "core::fingerprint_cache::tests::eviction_writer_child",
                "--nocapture",
            ])
            .env("CODE_GRAPH_EVICTION_WRITER_CHILD", "1")
            .env("CODE_GRAPH_EVICTION_ROOT", root.path())
            .spawn()
            .unwrap();
        let status = loop {
            let observed = cache.get(&k);
            assert!(
                matches!(observed, None | Some(Cached::Fingerprint(7))),
                "a concurrent reader sees only a complete shard or a miss: {observed:?}"
            );
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
        };
        assert!(status.success());
    }

    #[test]
    fn eviction_writer_child() {
        if std::env::var_os("CODE_GRAPH_EVICTION_WRITER_CHILD").is_none() {
            return;
        }
        let target_root = PathBuf::from(std::env::var_os("CODE_GRAPH_EVICTION_ROOT").unwrap());
        let cache = FingerprintCache::open(&target_root, HashSet::new());
        let k = key("abc123", "target", FingerprintMode::Normalized);
        for _ in 0..128 {
            let _ = std::fs::remove_file(cache.directory.join(k.relative_shard()));
            cache.put(&k, Cached::Fingerprint(7));
        }
    }
}
