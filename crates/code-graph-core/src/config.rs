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

use crate::Language;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Helper for `#[serde(default = "...")]` on `bool` fields whose documented
/// default is `true`. Plain `#[serde(default)]` would give `false`.
fn default_true() -> bool {
    true
}

/// Top-level project configuration loaded from `<root>/.code-graph.toml`.
///
/// All sections are `#[serde(default)]` so an empty file or a file that
/// omits a section still produces a valid config — every field has a
/// documented default.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RootConfig {
    #[serde(default)]
    pub discovery: DiscoveryConfig,
    #[serde(default)]
    pub parsing: ParsingConfig,
    #[serde(default)]
    pub cpp: CppConfig,
    #[serde(default)]
    pub extensions: ExtensionsConfig,
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

/// C++-specific knobs. Currently the only field is `macro_strip` — a list of
/// identifier tokens (typically API-export macros like `CORE_API`) that are
/// blanked out of C++ source bytes before tree-sitter parses them.
///
/// **Empty-string entries are filtered at load time.** An empty pattern would
/// match every byte position with zero advancement and infinite-loop the
/// substitution scan in production. [`RootConfig::load`] drains empty entries
/// and warns once per drop. The substitution algorithm (Phase 1.2) is allowed
/// to assume every pattern has length > 0.
///
/// The field is `Vec<String>` (not `Vec<&'static str>`); patterns are checked
/// for emptiness only — non-identifier-character patterns are not validated
/// here because the substitution layer does literal byte-equality matching.
/// See `Designs/CppMacroStrip/README.md` Decision 7 and Error Handling.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CppConfig {
    /// Identifier tokens to remove from C++ source bytes before tree-sitter
    /// parses them. Empty by default. Empty-string entries are filtered out
    /// at load time (see [`RootConfig::load`]).
    #[serde(default)]
    pub macro_strip: Vec<String>,
}

/// Per-language file-extension overrides.
///
/// Layered on top of each plugin's built-in extension list (e.g. C++'s
/// `.cpp/.cc/.cxx/.c/.h/.hpp/.hxx`). Three behaviors:
///
/// - **`<lang>` lists** add extensions to that language's claim. A file
///   whose extension matches `[extensions].cpp` is dispatched to the C++
///   plugin even if the C++ plugin's defaults wouldn't have claimed it.
/// - **A user addition silently wins over a default-claim collision.** If
///   `[extensions].python = [".h"]` and the C++ plugin's defaults also
///   claim `.h`, `.h` files dispatch to Python. (The user wrote the
///   override deliberately.) If two `[extensions].<lang>` lists both
///   claim the same extension, that's a load-time error — there's no
///   principled tiebreak.
/// - **`disabled` lists** suppress extensions entirely. A file whose
///   extension is in `disabled` is dropped at discovery time regardless
///   of which plugin or override would otherwise claim it. `disabled`
///   wins over both defaults and additions.
///
/// Each entry must start with `.` and is lowercased at load time.
/// Empty-string entries are dropped at load time with an `eprintln!`
/// notice (matching the [`CppConfig::macro_strip`] pattern).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExtensionsConfig {
    /// Extensions to skip during discovery, regardless of which language
    /// would claim them.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Additional extensions claimed by the C++ plugin.
    #[serde(default)]
    pub cpp: Vec<String>,
    /// Additional extensions claimed by the Rust plugin.
    #[serde(default)]
    pub rust: Vec<String>,
    /// Additional extensions claimed by the Go plugin.
    #[serde(default)]
    pub go: Vec<String>,
    /// Additional extensions claimed by the Python plugin.
    #[serde(default)]
    pub python: Vec<String>,
}

impl ExtensionsConfig {
    /// Look up an additional-claim extension. Returns the language whose
    /// `[extensions].<lang>` list contains `ext`, or `None`. The caller
    /// MUST pass `ext` in canonical form (lowercase, leading `.`) — the
    /// load-time normalization in [`RootConfig::load`] guarantees the
    /// stored entries are in this form.
    pub fn lookup_additional(&self, ext: &str) -> Option<Language> {
        if self.cpp.iter().any(|e| e == ext) {
            return Some(Language::Cpp);
        }
        if self.rust.iter().any(|e| e == ext) {
            return Some(Language::Rust);
        }
        if self.go.iter().any(|e| e == ext) {
            return Some(Language::Go);
        }
        if self.python.iter().any(|e| e == ext) {
            return Some(Language::Python);
        }
        None
    }

    /// Returns `true` if `ext` is in the global disabled list. `ext` must
    /// be in canonical form (lowercase, leading `.`).
    pub fn is_disabled(&self, ext: &str) -> bool {
        self.disabled.iter().any(|e| e == ext)
    }

    /// Iterate every `(label, list)` pair so load-time validation can scan
    /// each list uniformly. The label is the field name as it appears in
    /// `.code-graph.toml` (`"disabled"`, `"cpp"`, `"rust"`, `"go"`,
    /// `"python"`).
    fn lists_mut(
        &mut self,
    ) -> [(&'static str, &mut Vec<String>); 5] {
        [
            ("disabled", &mut self.disabled),
            ("cpp", &mut self.cpp),
            ("rust", &mut self.rust),
            ("go", &mut self.go),
            ("python", &mut self.python),
        ]
    }

    /// Iterate every additive `(label, list)` pair (excluding `disabled`)
    /// for cross-language collision detection.
    fn additive_lists(&self) -> [(&'static str, &Vec<String>); 4] {
        [
            ("cpp", &self.cpp),
            ("rust", &self.rust),
            ("go", &self.go),
            ("python", &self.python),
        ]
    }
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
    /// An entry in `[extensions].<list>` did not start with `.`. Without
    /// the leading dot the lookup path (`format!(".{ext}")`) would never
    /// match and the override would silently be a no-op.
    #[error("invalid extension {extension:?} in [extensions].{list}: must start with '.'")]
    ExtensionMissingDot {
        extension: String,
        list: &'static str,
    },
    /// Two `[extensions].<lang>` lists claimed the same extension. Unlike
    /// an additive vs. default collision (where the additive wins
    /// deliberately), there's no principled tiebreak between two
    /// additives, so this is a hard error.
    #[error(
        "extension {extension:?} is claimed by both [extensions].{first} and [extensions].{second}"
    )]
    ExtensionConflict {
        extension: String,
        first: &'static str,
        second: &'static str,
    },
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
        let mut parsed: Self = toml::from_str(&content)?;
        // Drain empty-string entries from `[cpp].macro_strip`. An empty
        // pattern would match every byte position with zero advancement and
        // infinite-loop the substitution scan in release builds — the
        // substitution algorithm (Phase 1.2) is allowed to assume every
        // pattern has length > 0, so the filter must run unconditionally.
        // We use `eprintln!` rather than `tracing::warn!` because this
        // workspace deliberately has no `tracing` dependency
        // (see `crates/code-graph-tools/src/handlers/watch.rs:461`).
        parsed.cpp.macro_strip.retain(|s| {
            let keep = !s.is_empty();
            if !keep {
                eprintln!(
                    "code-graph-mcp: dropping empty entry from .code-graph.toml [cpp].macro_strip"
                );
            }
            keep
        });

        // Normalize and validate `[extensions]` lists: drain empties (warn),
        // require leading dot, lowercase, and reject cross-additive
        // collisions. Done at load time so the dispatch hot path
        // (`language_for_path_with_config`) can do plain string compares.
        for (list_name, list) in parsed.extensions.lists_mut() {
            list.retain(|s| {
                let keep = !s.is_empty();
                if !keep {
                    eprintln!(
                        "code-graph-mcp: dropping empty entry from .code-graph.toml [extensions].{list_name}"
                    );
                }
                keep
            });
            for ext in list.iter() {
                if !ext.starts_with('.') {
                    return Err(ConfigError::ExtensionMissingDot {
                        extension: ext.clone(),
                        list: list_name,
                    });
                }
            }
            for ext in list.iter_mut() {
                ext.make_ascii_lowercase();
            }
        }
        // Cross-additive collision check. O(n²) over four typically-tiny
        // lists is fine; nobody adds hundreds of file extensions.
        let additive = parsed.extensions.additive_lists();
        for i in 0..additive.len() {
            for j in (i + 1)..additive.len() {
                for ext in additive[i].1 {
                    if additive[j].1.contains(ext) {
                        return Err(ConfigError::ExtensionConflict {
                            extension: ext.clone(),
                            first: additive[i].0,
                            second: additive[j].0,
                        });
                    }
                }
            }
        }

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

    // --- CppConfig tests (CppMacroStrip Phase 1.1) -------------------------

    #[test]
    fn cpp_config_default_is_empty() {
        // Zero-config users see an empty `macro_strip` list. The substitution
        // layer (Phase 1.2) short-circuits on empty list to `Cow::Borrowed`.
        let cfg = RootConfig::default();
        assert!(
            cfg.cpp.macro_strip.is_empty(),
            "default macro_strip must be empty (opt-in), got: {:?}",
            cfg.cpp.macro_strip
        );
    }

    #[test]
    fn cpp_section_absent_yields_default() {
        // Backward compatibility: every existing `.code-graph.toml` in the
        // wild has no `[cpp]` section. Loading must produce an empty
        // `macro_strip` with no error.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[discovery]\nmax_threads = 0\n[parsing]\nmax_threads = 0\n",
        )
        .unwrap();
        let cfg =
            RootConfig::load(dir.path()).expect("config without [cpp] section must load cleanly");
        assert!(
            cfg.cpp.macro_strip.is_empty(),
            "absent [cpp] section must default to empty macro_strip"
        );
    }

    #[test]
    fn cpp_macro_strip_filters_empty_strings() {
        // Anti-regression for the infinite-loop risk documented in
        // Designs/CppMacroStrip Error Handling. An empty pattern would
        // advance 0 bytes per iteration in the substitution scan; the filter
        // at config-load is the *only* safe place to enforce non-emptiness.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip = [\"\", \"CORE_API\", \"\"]\n",
        )
        .unwrap();
        let cfg = RootConfig::load(dir.path()).expect("load must succeed even with empty entries");
        assert_eq!(
            cfg.cpp.macro_strip,
            vec!["CORE_API".to_string()],
            "empty entries must be drained, leaving only valid patterns"
        );
    }

    #[test]
    fn cpp_macro_strip_empty_array_no_warnings() {
        // Explicit `macro_strip = []` is the same as omitting the section —
        // produces an empty list and (implicitly) emits no warnings. We can't
        // capture stderr portably without test infrastructure, so we verify
        // the resulting Vec and that load succeeds.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip = []\n",
        )
        .unwrap();
        let cfg = RootConfig::load(dir.path()).expect("[cpp] with empty array must load cleanly");
        assert!(
            cfg.cpp.macro_strip.is_empty(),
            "explicit empty array must yield empty macro_strip"
        );
    }

    // --- ExtensionsConfig tests --------------------------------------------

    #[test]
    fn extensions_config_default_is_empty() {
        let cfg = RootConfig::default();
        assert!(cfg.extensions.disabled.is_empty());
        assert!(cfg.extensions.cpp.is_empty());
        assert!(cfg.extensions.rust.is_empty());
        assert!(cfg.extensions.go.is_empty());
        assert!(cfg.extensions.python.is_empty());
    }

    #[test]
    fn extensions_section_absent_yields_default() {
        // Backward compatibility: `.code-graph.toml` files without an
        // `[extensions]` section must load cleanly with empty overrides.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".code-graph.toml"), "[discovery]\n").unwrap();
        let cfg = RootConfig::load(dir.path()).expect("load without [extensions]");
        assert!(cfg.extensions.disabled.is_empty());
        assert!(cfg.extensions.cpp.is_empty());
    }

    #[test]
    fn extensions_additive_lists_lookup_returns_correct_language() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            r#"
[extensions]
cpp = [".cu", ".inl"]
python = [".pyx"]
"#,
        )
        .unwrap();
        let cfg = RootConfig::load(dir.path()).expect("load valid additive lists");
        assert_eq!(cfg.extensions.lookup_additional(".cu"), Some(Language::Cpp));
        assert_eq!(cfg.extensions.lookup_additional(".inl"), Some(Language::Cpp));
        assert_eq!(
            cfg.extensions.lookup_additional(".pyx"),
            Some(Language::Python)
        );
        assert_eq!(cfg.extensions.lookup_additional(".rs"), None);
    }

    #[test]
    fn extensions_disabled_list_blocks_dispatch() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            r#"
[extensions]
disabled = [".h"]
"#,
        )
        .unwrap();
        let cfg = RootConfig::load(dir.path()).expect("load valid disabled list");
        assert!(cfg.extensions.is_disabled(".h"));
        assert!(!cfg.extensions.is_disabled(".cpp"));
    }

    #[test]
    fn extensions_normalize_to_lowercase_at_load() {
        // Users may write `.CU` or `.PyX`; lookup is always lowercase, so
        // normalization happens once at load.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            r#"
[extensions]
cpp = [".CU"]
disabled = [".PNG"]
"#,
        )
        .unwrap();
        let cfg = RootConfig::load(dir.path()).expect("load mixed-case entries");
        assert_eq!(cfg.extensions.cpp, vec![".cu".to_string()]);
        assert_eq!(cfg.extensions.disabled, vec![".png".to_string()]);
    }

    #[test]
    fn extensions_missing_leading_dot_errors() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            r#"
[extensions]
cpp = ["cu"]
"#,
        )
        .unwrap();
        let err = RootConfig::load(dir.path()).expect_err("dotless extension must error");
        match err {
            ConfigError::ExtensionMissingDot { extension, list } => {
                assert_eq!(extension, "cu");
                assert_eq!(list, "cpp");
            }
            other => panic!("expected ExtensionMissingDot, got: {other:?}"),
        }
    }

    #[test]
    fn extensions_cross_additive_conflict_errors() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            r#"
[extensions]
cpp = [".x"]
python = [".x"]
"#,
        )
        .unwrap();
        let err = RootConfig::load(dir.path()).expect_err("cross-additive conflict must error");
        match err {
            ConfigError::ExtensionConflict {
                extension,
                first,
                second,
            } => {
                assert_eq!(extension, ".x");
                assert_eq!(first, "cpp");
                assert_eq!(second, "python");
            }
            other => panic!("expected ExtensionConflict, got: {other:?}"),
        }
    }

    #[test]
    fn extensions_disabled_overlapping_additive_is_silent_and_disabled_wins() {
        // Documenting the precedence: if `.cu` is in BOTH `cpp` and
        // `disabled`, the load succeeds (no conflict error — `disabled` is
        // not in the additive collision check) and `is_disabled` returns
        // true. The dispatch in `language_for_path_with_config` checks
        // `is_disabled` before `lookup_additional`, so the file is dropped.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            r#"
[extensions]
cpp = [".cu"]
disabled = [".cu"]
"#,
        )
        .unwrap();
        let cfg = RootConfig::load(dir.path()).expect("disabled vs additive overlap is allowed");
        assert!(cfg.extensions.is_disabled(".cu"));
        assert_eq!(cfg.extensions.lookup_additional(".cu"), Some(Language::Cpp));
    }

    #[test]
    fn extensions_empty_entries_dropped() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            r#"
[extensions]
cpp = ["", ".cu", ""]
disabled = [""]
"#,
        )
        .unwrap();
        let cfg = RootConfig::load(dir.path()).expect("empty entries must be dropped, not error");
        assert_eq!(cfg.extensions.cpp, vec![".cu".to_string()]);
        assert!(cfg.extensions.disabled.is_empty());
    }

    #[test]
    fn cpp_macro_strip_preserves_order() {
        // The filter uses `Vec::retain` which preserves the relative order of
        // surviving elements. Order is not algorithmically required for
        // correctness (the whole-word check makes prefix-overlap order-safe
        // — see Designs/CppMacroStrip Architecture), but preserving the
        // user's listed order is the principle of least surprise.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip = [\"B\", \"A\", \"C\"]\n",
        )
        .unwrap();
        let cfg = RootConfig::load(dir.path()).expect("load must succeed");
        assert_eq!(
            cfg.cpp.macro_strip,
            vec!["B".to_string(), "A".to_string(), "C".to_string()],
            "macro_strip must preserve user-listed order"
        );
    }
}
