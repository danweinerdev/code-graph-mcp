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
//!
//! Sidecar entries are untrusted, same-local-user project state (D-0014), not
//! a cross-account security boundary. Static symlink/reparse and special-file
//! entries are rejected before use. Safe portable Rust cannot hold every path
//! component stable between metadata inspection and a later filesystem call,
//! so a same-user concurrent replacement race remains best-effort only; the
//! cache's miss/no-op behavior keeps that race non-authoritative.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs::{self, FileType, Metadata, OpenOptions};
use std::hash::Hasher;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

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
/// A shard holds one short framed key and one cached outcome. One MiB leaves
/// ample room for valid keys while preventing a cache file from driving an
/// unbounded allocation before JSON deserialization.
const MAX_SHARD_BYTES: u64 = 1024 * 1024;
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct CacheRootState {
    maintenance_opened: AtomicBool,
    successful_writes: AtomicU64,
    #[cfg(test)]
    maintenance_scans: AtomicU64,
}

impl CacheRootState {
    fn new() -> Self {
        Self {
            maintenance_opened: AtomicBool::new(false),
            successful_writes: AtomicU64::new(0),
            #[cfg(test)]
            maintenance_scans: AtomicU64::new(0),
        }
    }
}

/// Sidecar lifecycle state is shared by every history request for one
/// canonical project root. Keeping the `Arc` in this process-wide map makes
/// both open-time recovery and the successful-write cadence root-wide rather
/// than request-local.
static CACHE_ROOT_STATES: OnceLock<Mutex<HashMap<PathBuf, Arc<CacheRootState>>>> = OnceLock::new();

fn cache_root_state(root: &Path) -> Arc<CacheRootState> {
    let mut states = CACHE_ROOT_STATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    Arc::clone(
        states
            .entry(root.to_path_buf())
            .or_insert_with(|| Arc::new(CacheRootState::new())),
    )
}

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
    project_root: Option<PathBuf>,
    directory: PathBuf,
    protected_shards: Arc<HashSet<PathBuf>>,
    root_state: Arc<CacheRootState>,
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
        let project_root = fs::canonicalize(project_root)
            .ok()
            .filter(|root| is_safe_directory(root));
        let root_state = project_root
            .as_deref()
            .map(cache_root_state)
            .unwrap_or_else(|| Arc::new(CacheRootState::new()));
        let cache = Self {
            directory: project_root
                .as_deref()
                .map(|root| root.join(".code-graph").join("fingerprints"))
                .unwrap_or_default(),
            project_root,
            protected_shards: Arc::new(protected_shards),
            root_state,
            limits: DEFAULT_LIMITS,
        };
        cache.maintain_on_open();
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
        let path = self.shard_path(key)?;
        if !self.safe_existing_shard_parent(key.relative_shard()) {
            return None;
        }
        let shard = self.read_shard(&path)?;
        if shard.version != SHARD_FORMAT_VERSION || shard.key != key.stored {
            remove_regular_file(&path);
            return None;
        }
        Some(Cached::from(&shard.outcome))
    }

    /// Record one entry with an atomic temp+rename publish. Failures are
    /// ignored — the cache is never authoritative.
    pub fn put(&self, key: &FingerprintKey, value: Cached) {
        let Some(path) = self.shard_path(key) else {
            return;
        };
        let shard = StoredShard {
            version: SHARD_FORMAT_VERSION,
            key: key.stored.clone(),
            outcome: value.into(),
        };
        let Ok(encoded) = serde_json::to_vec(&shard) else {
            return;
        };
        let Some(parent) = self.ensure_shard_parent(key.relative_shard()) else {
            return;
        };
        // Process id alone is not enough: two walks in one process writing
        // the same shard would share a temp path and could publish a torn
        // file (self-healing, but avoidable for the cost of a counter).
        let sequence = WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp = path.with_extension(format!("tmp-{}-{sequence}", std::process::id()));
        if !self.publish_shard(&path, &parent, &temp, &encoded) {
            return;
        }
        let writes = self
            .root_state
            .successful_writes
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        if writes.is_multiple_of(self.limits.write_interval) {
            self.maintain();
        }
    }

    fn read_shard(&self, path: &Path) -> Option<StoredShard> {
        let metadata = fs::symlink_metadata(path).ok()?;
        if !is_safe_regular_file(&metadata) || metadata.len() > MAX_SHARD_BYTES {
            return None;
        }
        let file = OpenOptions::new().read(true).open(path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_SHARD_BYTES {
            return None;
        }
        let capacity = usize::try_from(metadata.len()).ok()?;
        let mut bytes = Vec::with_capacity(capacity);
        file.take(MAX_SHARD_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_SHARD_BYTES {
            return None;
        }
        match serde_json::from_slice(&bytes) {
            Ok(shard) => Some(shard),
            Err(_) => {
                // Corrupt shard: delete and recompute rather than error
                // (AC-46). A failed delete just means another miss later.
                remove_regular_file(path);
                None
            }
        }
    }

    fn shard_path(&self, key: &FingerprintKey) -> Option<PathBuf> {
        let relative = key.relative_shard();
        if self.project_root.is_none() || !is_v2_relative_shard(relative) {
            return None;
        }
        Some(self.directory.join(relative))
    }

    fn ensure_shard_parent(&self, relative: &Path) -> Option<PathBuf> {
        if !is_v2_relative_shard(relative) {
            return None;
        }
        let root = self.project_root.as_deref()?;
        if !is_safe_directory(root) {
            return None;
        }
        let code_graph = root.join(".code-graph");
        let v2 = self.directory.join(SHARD_DIRECTORY);
        let prefix = relative.parent()?.file_name()?;
        if !ensure_safe_directory(&code_graph)
            || !ensure_safe_directory(&self.directory)
            || !ensure_safe_directory(&v2)
        {
            return None;
        }
        let parent = v2.join(prefix);
        ensure_safe_directory(&parent).then_some(parent)
    }

    fn safe_existing_shard_parent(&self, relative: &Path) -> bool {
        let Some(prefix) = relative.parent().and_then(Path::file_name) else {
            return false;
        };
        self.safe_shard_parent(&self.directory.join(SHARD_DIRECTORY).join(prefix))
    }

    fn safe_shard_parent(&self, parent: &Path) -> bool {
        let v2 = self.directory.join(SHARD_DIRECTORY);
        self.safe_existing_cache_directory()
            && parent.parent() == Some(v2.as_path())
            && is_safe_directory(&v2)
            && is_safe_directory(parent)
    }

    /// Publish one already-encoded shard through a unique, exclusively
    /// created sibling temporary. `temp` must be a sibling of `path` so this
    /// remains an atomic replacement on supported filesystems.
    fn publish_shard(&self, path: &Path, parent: &Path, temp: &Path, encoded: &[u8]) -> bool {
        if temp.parent() != Some(parent)
            || !self.safe_shard_parent(parent)
            || !is_regular_file_or_missing(path)
        {
            return false;
        }
        let Ok(mut file) = OpenOptions::new().write(true).create_new(true).open(temp) else {
            // `create_new` refuses an existing temporary entry, including a
            // planted symlink, without opening or truncating its target.
            return false;
        };
        if file.write_all(encoded).is_err() || file.flush().is_err() {
            drop(file);
            remove_regular_file(temp);
            return false;
        }
        drop(file);
        if !self.safe_shard_parent(parent) || !is_regular_file_or_missing(path) {
            remove_regular_file(temp);
            return false;
        }
        if fs::rename(temp, path).is_err() {
            remove_regular_file(temp);
            return false;
        }
        true
    }

    fn maintain_on_open(&self) {
        if !self
            .root_state
            .maintenance_opened
            .swap(true, Ordering::AcqRel)
        {
            self.maintain();
        }
    }

    fn maintain(&self) {
        if !self.safe_existing_cache_directory() {
            return;
        }
        let lock_path = self.directory.join(MAINTENANCE_LOCK);
        let Some(lock) = open_maintenance_lock(&lock_path) else {
            return;
        };
        if lock.try_lock().is_err() {
            return;
        }
        self.maintain_locked();
        let _ = lock.unlock();
    }

    fn safe_existing_cache_directory(&self) -> bool {
        let Some(root) = self.project_root.as_deref() else {
            return false;
        };
        is_safe_directory(root)
            && is_safe_directory(&root.join(".code-graph"))
            && is_safe_directory(&self.directory)
    }

    fn maintain_locked(&self) {
        #[cfg(test)]
        self.root_state
            .maintenance_scans
            .fetch_add(1, Ordering::Relaxed);
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
            if remove_regular_file(&shard.path) {
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

    #[cfg(test)]
    fn maintenance_scan_count(&self) -> u64 {
        self.root_state.maintenance_scans.load(Ordering::Relaxed)
    }
}

/// Enumerate only the two recognized completed-shard layouts: legacy v1
/// `??.json` files at the sidecar root and v2 `??/<hash>.json` files. The
/// scan never recurses into arbitrary directory trees, and every traversed
/// directory/file must be a real entry rather than a symlink or reparse point.
fn collect_completed_shards(root: &Path, output: &mut Vec<CompletedShard>) {
    collect_shard_files(root, root, is_legacy_shard_name, output);
    let v2 = root.join(SHARD_DIRECTORY);
    if !is_safe_directory(&v2) {
        return;
    }
    let Ok(prefixes) = std::fs::read_dir(&v2) else {
        return;
    };
    for prefix in prefixes.flatten() {
        let path = prefix.path();
        if !is_hex_name(&prefix.file_name(), 2) || !is_safe_directory(&path) {
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
        if !is_safe_regular_file_type(&file_type) || !accepts_name(&entry.file_name()) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !is_safe_regular_file(&metadata) {
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

fn ensure_safe_directory(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata) => is_safe_directory_metadata(&metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).is_ok()
                && fs::symlink_metadata(path)
                    .map(|metadata| is_safe_directory_metadata(&metadata))
                    .unwrap_or(false)
        }
        Err(_) => false,
    }
}

fn is_safe_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| is_safe_directory_metadata(&metadata))
        .unwrap_or(false)
}

fn is_safe_directory_metadata(metadata: &Metadata) -> bool {
    let file_type = metadata.file_type();
    file_type.is_dir() && !is_link_or_reparse(metadata)
}

fn is_safe_regular_file(metadata: &Metadata) -> bool {
    metadata.file_type().is_file() && !is_link_or_reparse(metadata)
}

fn is_safe_regular_file_type(file_type: &FileType) -> bool {
    file_type.is_file() && !file_type.is_symlink()
}

fn is_regular_file_or_missing(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata) => is_safe_regular_file(&metadata),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

fn remove_regular_file(path: &Path) -> bool {
    if !is_regular_file_or_missing(path) {
        return false;
    }
    match fs::remove_file(path) {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

fn open_maintenance_lock(path: &Path) -> Option<std::fs::File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_safe_regular_file(&metadata) => {
            OpenOptions::new().read(true).write(true).open(path).ok()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .ok(),
        _ => None,
    }
}

fn is_v2_relative_shard(relative: &Path) -> bool {
    let mut components = relative.components();
    let Some(std::path::Component::Normal(directory)) = components.next() else {
        return false;
    };
    let Some(std::path::Component::Normal(prefix)) = components.next() else {
        return false;
    };
    let Some(std::path::Component::Normal(file)) = components.next() else {
        return false;
    };
    components.next().is_none()
        && directory == SHARD_DIRECTORY
        && is_hex_name(prefix, 2)
        && is_v2_shard_name(file)
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
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
        let mut cache = cache(root);
        cache.protected_shards = Arc::new(protected_shards);
        cache.limits = limits;
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
    fn oversized_regular_shard_is_a_prompt_cache_miss_without_deserialization() {
        let root = TempDir::new().unwrap();
        let cache = cache(&root);
        let k = key("abc123", "target", FingerprintMode::Normalized);
        let path = cache.directory.join(k.relative_shard());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![b'x'; MAX_SHARD_BYTES as usize + 1]).unwrap();

        assert_eq!(cache.get(&k), None);
        assert!(path.exists(), "an oversized shard is rejected, not decoded");
    }

    #[cfg(unix)]
    #[test]
    fn non_regular_shard_is_rejected_without_opening_it() {
        use std::os::unix::net::UnixListener;

        let root = TempDir::new().unwrap();
        let cache = cache(&root);
        let k = key("abc123", "target", FingerprintMode::Normalized);
        let path = cache.directory.join(k.relative_shard());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _listener = UnixListener::bind(&path).unwrap();

        let started = Instant::now();
        assert_eq!(cache.get(&k), None);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a socket shard must be rejected by no-follow metadata inspection"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_shard_directory_rejects_reads_and_writes() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let cache = cache(&root);
        let k = key("abc123", "target", FingerprintMode::Normalized);
        let relative = k.relative_shard();
        let prefix = relative.parent().unwrap().file_name().unwrap();
        let outside_shard = outside.path().join(relative.file_name().unwrap());
        std::fs::write(&outside_shard, b"outside sentinel").unwrap();
        let v2 = cache.directory.join(SHARD_DIRECTORY);
        std::fs::create_dir_all(&v2).unwrap();
        symlink(outside.path(), v2.join(prefix)).unwrap();

        assert_eq!(cache.get(&k), None, "read must not follow shard directory");
        cache.put(&k, Cached::Fingerprint(42));
        assert_eq!(
            std::fs::read(&outside_shard).unwrap(),
            b"outside sentinel",
            "write must not follow shard directory"
        );
    }

    #[test]
    fn publish_requires_an_exclusive_temporary_file() {
        let root = TempDir::new().unwrap();
        let cache = cache(&root);
        let k = key("abc123", "target", FingerprintMode::Normalized);
        let path = cache.shard_path(&k).unwrap();
        let parent = cache.ensure_shard_parent(k.relative_shard()).unwrap();
        let temp = parent.join("fixed.tmp");
        std::fs::write(&temp, b"preexisting temporary").unwrap();

        assert!(!cache.publish_shard(&path, &parent, &temp, b"replacement"));
        assert_eq!(std::fs::read(&temp).unwrap(), b"preexisting temporary");
        assert!(!path.exists(), "an occupied temp cannot publish a shard");
    }

    #[cfg(unix)]
    #[test]
    fn publish_does_not_follow_a_preexisting_temp_symlink() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let cache = cache(&root);
        let k = key("abc123", "target", FingerprintMode::Normalized);
        let path = cache.shard_path(&k).unwrap();
        let parent = cache.ensure_shard_parent(k.relative_shard()).unwrap();
        let temp = parent.join("fixed.tmp");
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"outside sentinel").unwrap();
        symlink(&sentinel, &temp).unwrap();

        assert!(!cache.publish_shard(&path, &parent, &temp, b"replacement"));
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"outside sentinel");
        assert!(
            temp.is_symlink(),
            "failed publication retains the foreign temp entry"
        );
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
    fn repeated_open_scans_each_cache_root_at_most_once_per_process() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join(".code-graph/fingerprints");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("aa.json"), b"legacy shard").unwrap();

        let first = cache(&root);
        assert_eq!(
            first.maintenance_scan_count(),
            1,
            "first open recovers stale state"
        );
        let second = cache(&root);
        assert_eq!(
            second.maintenance_scan_count(),
            1,
            "later history requests reuse the root-wide open state"
        );
    }

    #[test]
    fn successful_write_maintenance_cadence_is_shared_by_cache_root() {
        let root = TempDir::new().unwrap();
        let limits = MaintenanceLimits {
            high_bytes: u64::MAX,
            low_bytes: u64::MAX,
            high_shards: usize::MAX,
            low_shards: usize::MAX,
            write_interval: 2,
        };
        let mut first = cache(&root);
        first.limits = limits;
        first.put(
            &key("write-1", "target", FingerprintMode::Normalized),
            Cached::Fingerprint(1),
        );
        assert_eq!(first.maintenance_scan_count(), 0);

        let mut second = cache(&root);
        second.limits = limits;
        second.put(
            &key("write-2", "target", FingerprintMode::Normalized),
            Cached::Fingerprint(2),
        );
        assert_eq!(
            second.maintenance_scan_count(),
            1,
            "the second successful write across two handles triggers maintenance"
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
