//! Root configuration for an indexed project.
//!
//! Read from `<root>/.code-graph.toml`. Missing file → [`RootConfig::default`].
//! Parse failure → [`ConfigError::Toml`] (we never silently fall back, since a
//! typo in a thread-count is the kind of silent perf-degradation that wastes
//! hours — see Phase 1.3 design notes / Decision 8).
//!
//! After loading, call [`RootConfig::resolve_concurrency`] exactly once to
//! materialize any `0 = auto` values against
//! [`std::thread::available_parallelism`] and clamp over-cap pinned values
//! to the host's logical CPU count.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Helper for `#[serde(default = "...")]` on `bool` fields whose documented
/// default is `true`. Plain `#[serde(default)]` would give `false`.
fn default_true() -> bool {
    true
}

/// Top-level project configuration loaded from `<root>/.code-graph.toml`.
///
/// Both sections are `#[serde(default)]` so an empty file or a file that
/// omits one section still produces a valid config — every field has a
/// documented default.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RootConfig {
    #[serde(default)]
    pub discovery: DiscoveryConfig,
    #[serde(default)]
    pub parsing: ParsingConfig,
}

/// Discovery walker tunables. Controls how source files are found and which
/// ones are excluded.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DiscoveryConfig {
    /// Parallelism for the source-discovery walker. `0` means auto (resolved
    /// to `available_parallelism()` by [`RootConfig::resolve_concurrency`]).
    /// Values above the cap are clamped with a warning.
    #[serde(default)]
    pub max_threads: usize,
    /// If `true`, the discovery walker honors `.gitignore`, `.ignore`, and
    /// global ignore files (matches `ignore::WalkBuilder` defaults). Default
    /// is `true` — matches Go's behavior.
    #[serde(default = "default_true")]
    pub respect_gitignore: bool,
    /// If `true`, the discovery walker follows symlinks. Defaults to `false`
    /// to match the Go implementation and avoid cycles.
    #[serde(default)]
    pub follow_symlinks: bool,
    /// Additional glob patterns excluded from discovery, layered on top of
    /// gitignore handling.
    #[serde(default)]
    pub extra_ignore: Vec<String>,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            max_threads: 0,
            respect_gitignore: true,
            follow_symlinks: false,
            extra_ignore: Vec::new(),
        }
    }
}

/// Parsing pool tunables. Controls how many threads are spawned for the
/// rayon parsing pool.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ParsingConfig {
    /// Parallelism for the parsing pool. `0` means auto (resolved to
    /// `available_parallelism()` by [`RootConfig::resolve_concurrency`]).
    /// Values above the cap are clamped with a warning.
    #[serde(default)]
    pub max_threads: usize,
}

/// Errors returned by [`RootConfig::load`]. We deliberately split I/O from
/// TOML parse so callers can distinguish a missing/inaccessible file from a
/// malformed one.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// I/O error reading `<root>/.code-graph.toml` (excluding NotFound, which
    /// is treated as "absent" and yields [`RootConfig::default`]).
    #[error("failed to read .code-graph.toml: {0}")]
    Io(#[from] std::io::Error),
    /// TOML parse failure. Surfaced verbatim from the `toml` crate so the
    /// caller can include the row/column diagnostic in its error response.
    #[error("failed to parse .code-graph.toml: {0}")]
    Toml(#[from] toml::de::Error),
}

impl RootConfig {
    /// Load `<root>/.code-graph.toml`.
    ///
    /// - File missing → `Ok(RootConfig::default())`.
    /// - File present and valid TOML → `Ok(parsed)`.
    /// - File present but malformed → `Err(ConfigError::Toml)` (no fallback).
    /// - File present but unreadable (permissions, etc.) → `Err(ConfigError::Io)`.
    pub fn load(root: &Path) -> Result<Self, ConfigError> {
        let path = root.join(".code-graph.toml");
        let content = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => return Err(ConfigError::Io(e)),
        };
        let parsed: Self = toml::from_str(&content)?;
        Ok(parsed)
    }

    /// Resolve `0` → auto and clamp over-cap values to
    /// `available_parallelism()`. Returns a list of clamp warnings — one per
    /// pool whose pinned value exceeded the cap. The returned strings are
    /// suitable for surfacing through the `analyze_codebase` `warnings` array.
    ///
    /// Idempotent after the first call: once `max_threads` has been
    /// materialized to a non-zero value within `[1, cap]`, subsequent calls
    /// are no-ops and return an empty warnings vector.
    pub fn resolve_concurrency(&mut self) -> Vec<String> {
        let cap = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let mut warnings = Vec::new();
        let pools: [(&str, &mut usize); 2] = [
            ("discovery", &mut self.discovery.max_threads),
            ("parsing", &mut self.parsing.max_threads),
        ];
        for (label, n) in pools {
            if *n == 0 {
                *n = cap;
            } else if *n > cap {
                warnings.push(format!(
                    "{label}.max_threads={n} exceeds available_parallelism()={cap}; clamping to {cap}"
                ));
                *n = cap;
            }
        }
        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn cap() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }

    #[test]
    fn missing_file_returns_default() {
        let dir = TempDir::new().unwrap();
        let cfg = RootConfig::load(dir.path()).expect("missing file should yield default");
        // Defaults match the documented values.
        assert_eq!(cfg.discovery.max_threads, 0);
        assert!(cfg.discovery.respect_gitignore);
        assert!(!cfg.discovery.follow_symlinks);
        assert!(cfg.discovery.extra_ignore.is_empty());
        assert_eq!(cfg.parsing.max_threads, 0);
    }

    #[test]
    fn empty_file_yields_all_defaults() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".code-graph.toml"), "").unwrap();
        let cfg = RootConfig::load(dir.path()).expect("empty file should parse as default");
        let default = RootConfig::default();
        assert_eq!(cfg.discovery.max_threads, default.discovery.max_threads);
        assert_eq!(
            cfg.discovery.respect_gitignore,
            default.discovery.respect_gitignore
        );
        assert_eq!(
            cfg.discovery.follow_symlinks,
            default.discovery.follow_symlinks
        );
        assert_eq!(cfg.discovery.extra_ignore, default.discovery.extra_ignore);
        assert_eq!(cfg.parsing.max_threads, default.parsing.max_threads);
    }

    #[test]
    fn empty_sections_yield_section_defaults() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[discovery]\n[parsing]\n",
        )
        .unwrap();
        let cfg = RootConfig::load(dir.path()).expect("empty sections should parse");
        // Values within the empty sections fall back to the per-field defaults.
        assert_eq!(cfg.discovery.max_threads, 0);
        assert!(cfg.discovery.respect_gitignore);
        assert!(!cfg.discovery.follow_symlinks);
        assert!(cfg.discovery.extra_ignore.is_empty());
        assert_eq!(cfg.parsing.max_threads, 0);
    }

    #[test]
    fn valid_auto_resolves_to_cap() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[discovery]\nmax_threads = 0\n[parsing]\nmax_threads = 0\n",
        )
        .unwrap();
        let mut cfg = RootConfig::load(dir.path()).expect("valid auto config should parse");
        let warnings = cfg.resolve_concurrency();
        assert!(
            warnings.is_empty(),
            "auto values must not warn: {warnings:?}"
        );
        let c = cap();
        assert_eq!(cfg.discovery.max_threads, c);
        assert_eq!(cfg.parsing.max_threads, c);
    }

    #[test]
    fn pinned_within_cap_is_preserved() {
        let dir = TempDir::new().unwrap();
        // Pin at 1, which is always <= cap on every host.
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[discovery]\nmax_threads = 1\n[parsing]\nmax_threads = 1\n",
        )
        .unwrap();
        let mut cfg = RootConfig::load(dir.path()).expect("valid pinned config should parse");
        let warnings = cfg.resolve_concurrency();
        assert!(
            warnings.is_empty(),
            "pinned values within cap must not warn: {warnings:?}"
        );
        assert_eq!(cfg.discovery.max_threads, 1);
        assert_eq!(cfg.parsing.max_threads, 1);
    }

    #[test]
    fn over_cap_is_clamped_with_warning() {
        let dir = TempDir::new().unwrap();
        // usize::MAX / 2 is guaranteed to exceed available_parallelism() on any host.
        let huge = usize::MAX / 2;
        let toml = format!("[discovery]\nmax_threads = {huge}\n[parsing]\nmax_threads = {huge}\n");
        fs::write(dir.path().join(".code-graph.toml"), toml).unwrap();
        let mut cfg = RootConfig::load(dir.path()).expect("over-cap config should parse");
        let warnings = cfg.resolve_concurrency();
        assert_eq!(
            warnings.len(),
            2,
            "expected one warning per over-cap pool, got: {warnings:?}"
        );
        assert!(warnings[0].contains("discovery.max_threads"));
        assert!(warnings[0].contains("clamping"));
        assert!(warnings[1].contains("parsing.max_threads"));
        assert!(warnings[1].contains("clamping"));
        let c = cap();
        assert_eq!(cfg.discovery.max_threads, c);
        assert_eq!(cfg.parsing.max_threads, c);
    }

    #[test]
    fn malformed_toml_returns_error_no_fallback() {
        let dir = TempDir::new().unwrap();
        // Garbage that won't parse as TOML.
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[discovery\nmax_threads = not-a-number\n",
        )
        .unwrap();
        let err = RootConfig::load(dir.path())
            .expect_err("malformed TOML must error, not silently fall back to default");
        match err {
            ConfigError::Toml(_) => {}
            other => panic!("expected ConfigError::Toml, got: {other:?}"),
        }
    }

    #[test]
    fn resolve_concurrency_is_idempotent() {
        let mut cfg = RootConfig::default();
        let first = cfg.resolve_concurrency();
        assert!(first.is_empty());
        let snapshot = (cfg.discovery.max_threads, cfg.parsing.max_threads);
        let second = cfg.resolve_concurrency();
        assert!(
            second.is_empty(),
            "second call must not produce warnings: {second:?}"
        );
        assert_eq!(
            (cfg.discovery.max_threads, cfg.parsing.max_threads),
            snapshot
        );
    }

    #[test]
    fn over_cap_only_one_pool_warns_only_for_that_pool() {
        let dir = TempDir::new().unwrap();
        let huge = usize::MAX / 2;
        // Discovery over cap, parsing pinned at 1.
        let toml = format!("[discovery]\nmax_threads = {huge}\n[parsing]\nmax_threads = 1\n");
        fs::write(dir.path().join(".code-graph.toml"), toml).unwrap();
        let mut cfg = RootConfig::load(dir.path()).unwrap();
        let warnings = cfg.resolve_concurrency();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("discovery.max_threads"));
        assert_eq!(cfg.discovery.max_threads, cap());
        assert_eq!(cfg.parsing.max_threads, 1);
    }

    #[test]
    fn round_trip_serialize_deserialize() {
        // Confirms Serialize derive works (useful for snapshot tests later)
        // and that the schema is stable.
        let mut cfg = RootConfig::default();
        cfg.discovery.extra_ignore.push("**/vendor/**".to_string());
        let serialized = toml::to_string(&cfg).unwrap();
        let back: RootConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(back.discovery.extra_ignore, cfg.discovery.extra_ignore);
        assert_eq!(
            back.discovery.respect_gitignore,
            cfg.discovery.respect_gitignore
        );
    }
}
