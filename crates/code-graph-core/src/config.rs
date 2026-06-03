//! Root configuration for an indexed project.
//!
//! Read from `<root>/.code-graph.toml`. Missing file → [`RootConfig::default`].
//! Parse failure → [`ConfigError::Toml`] (we never silently fall back, since a
//! typo in a thread-count is the kind of silent perf-degradation that wastes
//! hours).
//!
//! After loading, call [`RootConfig::resolve_concurrency`] exactly once to
//! materialize any `0 = auto` values against
//! [`std::thread::available_parallelism`] and clamp over-cap pinned values
//! to the host's logical CPU count.

use crate::Language;
use serde::{Deserialize, Deserializer, Serialize};
use std::path::{Path, PathBuf};

/// Helper for `#[serde(default = "...")]` on `bool` fields whose documented
/// default is `true`. Plain `#[serde(default)]` would give `false`.
fn default_true() -> bool {
    true
}

/// Default value for `[response].max_bytes` — 100 KB. Sized to keep
/// individual MCP tool responses within a budget that the agent context
/// window can absorb without forcing aggressive summarization, while still
/// fitting a page of typical-size records (see `PaginatedResponseSizeSafety`
/// plan README, Decision 8).
pub const DEFAULT_RESPONSE_MAX_BYTES: usize = 102_400;

/// Helper for `#[serde(default = "...")]` on `ResponseConfig::max_bytes` so
/// that omitting the key (with the section present) still yields the
/// documented default.
fn default_response_max_bytes() -> usize {
    DEFAULT_RESPONSE_MAX_BYTES
}

/// Helper for `#[serde(default = "...")]` on [`MacroDefineType::keyword`]. The
/// documented default is `"struct"` (the overwhelmingly common engine pattern,
/// where the macro expands to a plain-old-data aggregate). Plain
/// `#[serde(default)]` would give an empty string, which
/// [`RootConfig::load`] would then reject as an invalid keyword.
fn default_macro_define_type_keyword() -> String {
    "struct".to_string()
}

/// Custom deserializer for `[response].max_bytes`. Rejects zero with a
/// clear error message — a budget of zero would make every paginated
/// handler return an empty page with `truncated=true`, which is
/// silently-broken behavior nobody would intend.
///
/// Negative integers and non-integer values are rejected by `toml`/`serde`
/// at the type-coercion layer (the field is `usize`), so this validator
/// only has to guard against the one in-range value that's still nonsense.
fn deserialize_response_max_bytes<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    let value = usize::deserialize(deserializer)?;
    if value == 0 {
        return Err(serde::de::Error::custom(
            "`[response].max_bytes` must be > 0",
        ));
    }
    Ok(value)
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
    #[serde(default)]
    pub response: ResponseConfig,
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

/// C++-specific knobs. `macro_strip` is whole-word identifier replacement;
/// `macro_strip_with_args` is parameterized-macro replacement (identifier +
/// balanced `(args)`). See `Designs/UeMacroSupport`.
///
/// **Empty-string entries are filtered at load time.** An empty pattern would
/// match every byte position with zero advancement and infinite-loop the
/// substitution scan in production. [`RootConfig::load`] drains empty entries
/// and warns once per drop. The downstream substitution algorithm is allowed
/// to assume every pattern has length > 0.
///
/// The fields are `Vec<String>` (not `Vec<&'static str>`); patterns are checked
/// for emptiness only — non-identifier-character patterns are not validated
/// here because the substitution layer does literal byte-equality matching.
/// See `Designs/CppMacroStrip/README.md` Decision 7 and Error Handling.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CppConfig {
    /// Identifier tokens to remove from C++ source bytes before tree-sitter
    /// parses them. Empty by default. Empty-string entries are filtered out
    /// at load time (see [`RootConfig::load`]). Within-list duplicates are
    /// NOT deduplicated — the substitution pass is idempotent, so duplicates
    /// produce extra work without changing output. (`macro_strip_with_args`
    /// dedups; the asymmetry is intentional but preserved because the
    /// `macro_strip` field shipped before the dedup convention was
    /// established.)
    #[serde(default)]
    pub macro_strip: Vec<String>,
    /// Parameterized macro tokens to remove from C++ source bytes before
    /// tree-sitter parses them; the identifier AND its trailing balanced
    /// `(args)` group are both blanked. Empty by default. Empty-string
    /// entries are filtered out at load time; within-list duplicates are
    /// silently deduplicated; tokens that appear in BOTH `macro_strip` and
    /// `macro_strip_with_args` are rejected with
    /// [`ConfigError::MacroStripConflict`] (see [`RootConfig::load`]).
    #[serde(default)]
    pub macro_strip_with_args: Vec<String>,
    /// Macros that DEFINE a top-level function via token-pasting.
    /// Each entry tells the C++ parser to synthesize a `Function`
    /// Symbol whenever it sees the macro invoked at namespace scope:
    /// `MACRO(NameToken)` → synthesize `<NameToken><suffix>` as a
    /// function with `Symbol.line` at the macro invocation. Common
    /// pattern in engine / SDK codebases that hide `_Release` /
    /// `_Get` / `_Set` function families behind a single macro for
    /// boilerplate elimination.
    ///
    /// Each entry carries:
    /// - `name` — the macro identifier (e.g. `"DECLARE_RELEASE_FN"`)
    /// - `arg` — zero-based index of the macro argument that names
    ///   the function (e.g. `0` for `DECLARE_RELEASE_FN(MyType)`)
    /// - `suffix` — optional string appended to the captured
    ///   identifier when forming the synthesized function name (e.g.
    ///   `"_Release"` for the canonical `<Type>_Release` pattern).
    ///   Empty / absent = no suffix.
    ///
    /// Opt-in: empty list is the default and produces no synthesis.
    /// Within-list duplicates are silently deduplicated by `name`
    /// (first-occurrence wins; the duplicate handling is separate
    /// from `macro_strip_with_args`'s `Vec<String>` dedup because
    /// here the dedup key is the `name` field of a struct).
    #[serde(default)]
    pub macro_define_function: Vec<MacroDefineFunction>,
    /// Macros that WRAP a `struct` / `class` definition via a
    /// parameterized expansion. Each entry tells the C++ parser to
    /// rewrite the macro invocation IN PLACE into the real C++ the
    /// macro would expand to, so tree-sitter parses the type natively
    /// — recovering the type symbol AND its members (methods, nested
    /// types) AND inheritance/call edges, not just a synthetic name.
    ///
    /// The canonical engine pattern is:
    /// ```text
    /// #define EXPORT_STRUCT(name, ...) struct CALL_API name { __VA_ARGS__ }
    /// EXPORT_STRUCT(Foo, (
    ///     int32_t bar;
    ///     void method();
    /// ));
    /// ```
    /// which is rewritten to `struct Foo { int32_t bar; void method(); };`.
    ///
    /// Each entry carries:
    /// - `name` — the macro identifier (e.g. `"EXPORT_STRUCT"`).
    /// - `name_arg` — zero-based index of the argument holding the
    ///   type NAME (default `0`).
    /// - `body_arg` — zero-based index of the argument holding the
    ///   member BODY; absent = the LAST argument (the common
    ///   `__VA_ARGS__` tail).
    /// - `keyword` — `"struct"` (default) or `"class"`; written in
    ///   place of the macro name. Any other value is rejected at load
    ///   time with [`ConfigError::MacroDefineTypeKeyword`].
    ///
    /// **Distinct from `macro_define_function`:** that one SYNTHESIZES
    /// a bare `Function` symbol from a token-pasting macro and does not
    /// touch the source bytes. This one EXPANDS the macro into real
    /// C++ in the preprocess pass so the full type body is parsed.
    ///
    /// Opt-in: empty list is the default and produces no expansion.
    /// Within-list duplicates are silently deduplicated by `name`
    /// (first-occurrence wins), mirroring `macro_define_function`.
    /// An empty `name` is rejected at load with
    /// [`ConfigError::MacroDefineTypeEmptyName`].
    #[serde(default)]
    pub macro_define_type: Vec<MacroDefineType>,
}

/// One entry in `[cpp].macro_define_function`. See [`CppConfig::macro_define_function`].
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct MacroDefineFunction {
    /// Macro identifier to watch for at namespace-scope invocations.
    pub name: String,
    /// Zero-based index of the macro argument that names the
    /// synthesized function.
    #[serde(default)]
    pub arg: usize,
    /// Optional suffix appended to the captured argument when
    /// forming the synthesized function name.
    #[serde(default)]
    pub suffix: String,
}

/// One entry in `[cpp].macro_define_type`. See [`CppConfig::macro_define_type`].
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct MacroDefineType {
    /// Macro identifier whose invocation wraps a struct/class
    /// definition (e.g. `"EXPORT_STRUCT"`). Rejected at load time if
    /// empty ([`ConfigError::MacroDefineTypeEmptyName`]).
    pub name: String,
    /// Zero-based index of the argument holding the type NAME.
    /// Defaults to `0` (the first argument).
    #[serde(default)]
    pub name_arg: usize,
    /// Zero-based index of the argument holding the member BODY.
    /// `None` (the default) means the LAST argument — the common
    /// `__VA_ARGS__` tail.
    #[serde(default)]
    pub body_arg: Option<usize>,
    /// The C++ keyword written in place of the macro name when
    /// expanding: `"struct"` (default) or `"class"`. Any other value
    /// is rejected at load time
    /// ([`ConfigError::MacroDefineTypeKeyword`]).
    #[serde(default = "default_macro_define_type_keyword")]
    pub keyword: String,
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
    /// Additional extensions claimed by the C# plugin.
    #[serde(default)]
    pub csharp: Vec<String>,
    /// Additional extensions claimed by the Java plugin.
    #[serde(default)]
    pub java: Vec<String>,
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
        if self.csharp.iter().any(|e| e == ext) {
            return Some(Language::CSharp);
        }
        if self.java.iter().any(|e| e == ext) {
            return Some(Language::Java);
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
    /// `"python"`, `"csharp"`, `"java"`).
    fn lists_mut(&mut self) -> [(&'static str, &mut Vec<String>); 7] {
        [
            ("disabled", &mut self.disabled),
            ("cpp", &mut self.cpp),
            ("rust", &mut self.rust),
            ("go", &mut self.go),
            ("python", &mut self.python),
            ("csharp", &mut self.csharp),
            ("java", &mut self.java),
        ]
    }

    /// Iterate every additive `(label, list)` pair (excluding `disabled`)
    /// for cross-language collision detection.
    fn additive_lists(&self) -> [(&'static str, &Vec<String>); 6] {
        [
            ("cpp", &self.cpp),
            ("rust", &self.rust),
            ("go", &self.go),
            ("python", &self.python),
            ("csharp", &self.csharp),
            ("java", &self.java),
        ]
    }
}

/// Response-shaping tunables. Controls the byte budget that paginated tool
/// handlers honor when materializing a page of results.
///
/// `max_bytes` is a soft per-response ceiling enforced by the
/// `byte_budget_take` helper in `code-graph-tools`. When a candidate record
/// would push the running serialized size over the budget, the page is cut
/// short with `truncated=true` and `next_offset` set so the caller can resume.
///
/// The default (`102_400` bytes = 100 KB) is sized to fit a page of
/// typical-size records under a single MCP tool response that an AI agent
/// can absorb without forcing summarization.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResponseConfig {
    /// Per-response byte budget consulted by paginated handlers. Default
    /// `102_400` (100 KB). Must be `> 0`; zero is rejected at load time
    /// with a clear error (a zero budget would silently return an empty
    /// page on every call).
    #[serde(
        default = "default_response_max_bytes",
        deserialize_with = "deserialize_response_max_bytes"
    )]
    pub max_bytes: usize,
}

// Cannot derive: max_bytes default is DEFAULT_RESPONSE_MAX_BYTES, not 0.
impl Default for ResponseConfig {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_RESPONSE_MAX_BYTES,
        }
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
    /// A token appears in BOTH `[cpp].macro_strip` and
    /// `[cpp].macro_strip_with_args`. Each list applies a different
    /// substitution rule (whole-word vs. identifier + balanced args), so
    /// listing the same token in both is ambiguous about the user's intent;
    /// there's no principled tiebreak.
    #[error("[cpp] macro '{token}' may not appear in both `macro_strip` and `macro_strip_with_args` (ambiguous strip target — remove it from one list or the other)")]
    MacroStripConflict { token: String },
    /// An entry in `[cpp].macro_define_type` had an empty `name`.
    /// Without a macro identifier the scanner has nothing to match,
    /// so the entry is meaningless — reject it rather than silently
    /// no-op (the user clearly intended to configure something).
    #[error("[cpp].macro_define_type entry has an empty `name` (every entry must name the macro to expand)")]
    MacroDefineTypeEmptyName,
    /// An entry in `[cpp].macro_define_type` had a `keyword` other
    /// than `struct` or `class`. The expansion writes this keyword
    /// verbatim in place of the macro name; any other value would
    /// produce invalid C++ that tree-sitter rejects.
    #[error("[cpp].macro_define_type entry '{name}' has invalid keyword {keyword:?}: must be \"struct\" or \"class\"")]
    MacroDefineTypeKeyword { name: String, keyword: String },
}

impl RootConfig {
    /// Discover and load the nearest `.code-graph.toml`, walking from
    /// `start` upward through every ancestor directory until one is
    /// found or the filesystem root is reached. Returns the loaded
    /// config alongside the **project root** — the directory containing
    /// the discovered toml, or `start` itself if no toml exists at any
    /// ancestor.
    ///
    /// The project root is the load-bearing piece of data this method
    /// surfaces beyond just the config: cache placement, indexing
    /// scope semantics, and the discovered-config-at-parent warning
    /// surface all key off it. Callers that don't need the root can
    /// destructure-and-discard (`let (cfg, _) = …`), but should
    /// generally surface it through `AnalyzeResult.warnings` so the
    /// user can see which toml actually applied.
    ///
    /// Discovery semantics:
    /// - **First match wins.** Nested tomls override their ancestors;
    ///   there is no merging across multiple files.
    /// - **No toml anywhere up to filesystem root** → returns
    ///   `(Self::default(), start.to_path_buf())`. The fall-through
    ///   project-root is the caller's invocation directory.
    /// - **Malformed toml** → `Err(ConfigError::Toml)` (no fallback;
    ///   present-but-broken is an explicit user error).
    /// - **Permission error** on an ancestor → `Err(ConfigError::Io)`.
    ///   Don't swallow these; a misconfigured ancestor is information
    ///   the caller needs.
    ///
    /// The walk uses `Path::parent()`, which on canonicalized paths
    /// gives resolved-target ancestry — symlinks behave correctly
    /// because `analyze_codebase` canonicalizes its input before
    /// calling this. Matches the convention shared by cargo,
    /// rustfmt, git, editorconfig, and npm: project-root config files
    /// are discovered by upward walk, never by exact-dir match.
    pub fn load(start: &Path) -> Result<(Self, PathBuf), ConfigError> {
        let mut search = Some(start);
        while let Some(dir) = search {
            let path = dir.join(".code-graph.toml");
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    let cfg = Self::parse_and_validate(&content)?;
                    return Ok((cfg, dir.to_path_buf()));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    search = dir.parent();
                }
                Err(e) => return Err(ConfigError::Io(e)),
            }
        }
        Ok((Self::default(), start.to_path_buf()))
    }

    /// Parse and validate `.code-graph.toml` content. Shared between
    /// [`load`]'s upward-walk hit path and tests that want to drive
    /// validation against a synthetic content string without touching
    /// the filesystem.
    fn parse_and_validate(content: &str) -> Result<Self, ConfigError> {
        let mut parsed: Self = toml::from_str(content)?;
        // Drain empty-string entries from `[cpp].macro_strip`. An empty
        // pattern would match every byte position with zero advancement and
        // infinite-loop the substitution scan in release builds — the
        // substitution algorithm is allowed to assume every pattern has
        // length > 0, so the filter must run unconditionally.
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

        // Parallel validation for `[cpp].macro_strip_with_args`. Two extra
        // steps relative to `macro_strip`:
        //   1. Drain empty-string entries (same per-drop `eprintln!` cadence
        //      as `macro_strip` above; the substitution algorithm requires
        //      every pattern to have length > 0).
        //   2. Silently deduplicate within-list duplicates while preserving
        //      first-occurrence order. A user repeating the same macro is
        //      almost always a paste-mistake, not an intentional weighting,
        //      and the downstream scan is idempotent — dedup is the principle
        //      of least surprise.
        //
        // Drain → dedup → cross-check. Drain-then-cross-check is the
        // canonical order: empty strings could superficially appear to be in
        // both lists before they're dropped.
        //
        // Case-sensitive throughout. C++ macro names are case-sensitive;
        // lowercasing would corrupt user config.
        parsed.cpp.macro_strip_with_args.retain(|s| {
            let keep = !s.is_empty();
            if !keep {
                eprintln!(
                    "code-graph-mcp: dropping empty entry from .code-graph.toml [cpp].macro_strip_with_args"
                );
            }
            keep
        });
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        parsed
            .cpp
            .macro_strip_with_args
            .retain(|s| seen.insert(s.clone()));

        // Cross-list conflict: a token in BOTH `macro_strip` and
        // `macro_strip_with_args` is ambiguous about the user's intent (each
        // list applies a different substitution rule). One offending token is
        // enough to block the load; we don't enumerate all hits.
        if let Some(token) = parsed
            .cpp
            .macro_strip
            .iter()
            .find(|t| parsed.cpp.macro_strip_with_args.contains(t))
        {
            return Err(ConfigError::MacroStripConflict {
                token: token.clone(),
            });
        }

        // Validate and dedup `[cpp].macro_define_type`. An empty `name` has
        // nothing for the scanner to match; an off-list `keyword` would
        // produce invalid C++ on expansion. Both are hard errors (the user
        // configured something concrete and got it wrong — silently
        // dropping the entry would hide a real misconfiguration). Within-
        // list duplicates dedup by `name` (first-occurrence wins), mirroring
        // `macro_define_function`'s struct-keyed dedup.
        for entry in &parsed.cpp.macro_define_type {
            if entry.name.is_empty() {
                return Err(ConfigError::MacroDefineTypeEmptyName);
            }
            if entry.keyword != "struct" && entry.keyword != "class" {
                return Err(ConfigError::MacroDefineTypeKeyword {
                    name: entry.name.clone(),
                    keyword: entry.keyword.clone(),
                });
            }
        }
        let mut seen_define_type: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        parsed
            .cpp
            .macro_define_type
            .retain(|e| seen_define_type.insert(e.name.clone()));

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
        // Cross-additive collision check. O(n²) over six typically-tiny
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

    // --- Upward-walk discovery ------------------------------------------

    /// Walk finds the toml at `start` itself (no walk needed). Sanity
    /// check that the trivial case still works.
    #[test]
    fn discover_returns_start_when_toml_at_start() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip = [\"MYLIB_API\"]\n",
        )
        .unwrap();
        let (cfg, root) =
            RootConfig::load(dir.path()).expect("toml at start dir must load with start as root");
        assert_eq!(
            cfg.cpp.macro_strip,
            vec!["MYLIB_API".to_string()],
            "loaded config must reflect the toml at start"
        );
        let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        assert_eq!(
            canonical_root, canonical_dir,
            "project root must equal the start dir when toml is found there"
        );
    }

    /// Walk finds the toml at the parent of `start`. The
    /// regression-bug scenario from `PossibleFix.txt`: user invokes
    /// at a subdir, toml lives one level up, discovery must locate
    /// it and return the parent as project root.
    #[test]
    fn discover_walks_up_one_level() {
        let dir = TempDir::new().unwrap();
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip = [\"PARENT_API\"]\n",
        )
        .unwrap();

        let (cfg, root) =
            RootConfig::load(&subdir).expect("walk-up-one must find the parent's toml");
        assert_eq!(
            cfg.cpp.macro_strip,
            vec!["PARENT_API".to_string()],
            "loaded config must come from the parent toml"
        );
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        let canonical_parent = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            canonical_root, canonical_parent,
            "project root must be the directory containing the toml"
        );
    }

    /// Walk finds the toml three levels up from `start`. Confirms the
    /// upward walk is not depth-limited.
    #[test]
    fn discover_walks_up_three_levels() {
        let dir = TempDir::new().unwrap();
        let deep = dir.path().join("a").join("b").join("c");
        fs::create_dir_all(&deep).unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip_with_args = [\"DEEP_REFLECT\"]\n",
        )
        .unwrap();

        let (cfg, root) = RootConfig::load(&deep).expect("walk must traverse multiple levels");
        assert_eq!(
            cfg.cpp.macro_strip_with_args,
            vec!["DEEP_REFLECT".to_string()]
        );
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        let canonical_parent = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(canonical_root, canonical_parent);
    }

    /// Walk respects first-match-wins: nested toml shadows its ancestor.
    /// The project boundary lives at the nearest toml, not the highest.
    #[test]
    fn discover_first_match_wins_nested_toml_shadows_ancestor() {
        let dir = TempDir::new().unwrap();
        let inner = dir.path().join("inner");
        fs::create_dir(&inner).unwrap();

        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip = [\"OUTER_API\"]\n",
        )
        .unwrap();
        fs::write(
            inner.join(".code-graph.toml"),
            "[cpp]\nmacro_strip = [\"INNER_API\"]\n",
        )
        .unwrap();

        let (cfg, root) = RootConfig::load(&inner).expect("nested toml must load");
        assert_eq!(
            cfg.cpp.macro_strip,
            vec!["INNER_API".to_string()],
            "inner toml's value must win — no merging with the ancestor"
        );
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        let canonical_inner = std::fs::canonicalize(&inner).unwrap();
        assert_eq!(
            canonical_root, canonical_inner,
            "project root must be the nearest toml's directory, not the outer one"
        );
    }

    /// No toml at start or any ancestor up to filesystem root → defaults
    /// returned with `start` itself as the project root. The fallback
    /// case for users who haven't created a `.code-graph.toml`.
    ///
    /// Relies on the test environment having no `.code-graph.toml` in
    /// any ancestor of the tempdir (typically `/tmp/.tmpXYZ/`). On a
    /// dev machine that happens to have one at `$HOME`, this test
    /// could surface that — which is precisely the cargo/git contract
    /// (an ancestor toml IS your project), so the assertion failure
    /// would be informative rather than a bug.
    #[test]
    fn discover_falls_back_to_defaults_when_no_toml_anywhere() {
        let dir = TempDir::new().unwrap();
        let (cfg, root) =
            RootConfig::load(dir.path()).expect("missing toml everywhere must yield default");
        let default = RootConfig::default();
        assert_eq!(cfg.cpp.macro_strip, default.cpp.macro_strip);
        assert_eq!(
            cfg.cpp.macro_strip_with_args,
            default.cpp.macro_strip_with_args
        );
        assert_eq!(
            root,
            dir.path().to_path_buf(),
            "no-toml fallback must use the start dir as project root"
        );
    }

    #[test]
    fn missing_file_returns_default() {
        let dir = TempDir::new().unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("missing file should yield default");
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
        let (cfg, _root) =
            RootConfig::load(dir.path()).expect("empty file should parse as default");
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
        let (cfg, _root) = RootConfig::load(dir.path()).expect("empty sections should parse");
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
        let (mut cfg, _root) =
            RootConfig::load(dir.path()).expect("valid auto config should parse");
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
        let (mut cfg, _root) =
            RootConfig::load(dir.path()).expect("valid pinned config should parse");
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
        let (mut cfg, _root) = RootConfig::load(dir.path()).expect("over-cap config should parse");
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
        let (mut cfg, _root) = RootConfig::load(dir.path()).unwrap();
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

    // --- CppConfig tests ---------------------------------------------------

    #[test]
    fn cpp_config_default_is_empty() {
        // Zero-config users see an empty `macro_strip` list. The substitution
        // layer short-circuits on empty list to `Cow::Borrowed`.
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
        let (cfg, _root) =
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
        let (cfg, _root) =
            RootConfig::load(dir.path()).expect("load must succeed even with empty entries");
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
        let (cfg, _root) =
            RootConfig::load(dir.path()).expect("[cpp] with empty array must load cleanly");
        assert!(
            cfg.cpp.macro_strip.is_empty(),
            "explicit empty array must yield empty macro_strip"
        );
    }

    // --- MacroDefineType tests ---------------------------------------------

    #[test]
    fn macro_define_type_default_is_empty() {
        let cfg = RootConfig::default();
        assert!(
            cfg.cpp.macro_define_type.is_empty(),
            "default macro_define_type must be empty (opt-in)"
        );
    }

    #[test]
    fn macro_define_type_keyword_defaults_to_struct() {
        // `keyword` omitted → defaults to "struct" via the serde default.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_define_type = [{ name = \"EXPORT_STRUCT\" }]\n",
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("load must succeed");
        assert_eq!(cfg.cpp.macro_define_type.len(), 1);
        let e = &cfg.cpp.macro_define_type[0];
        assert_eq!(e.name, "EXPORT_STRUCT");
        assert_eq!(e.name_arg, 0, "name_arg defaults to 0");
        assert_eq!(e.body_arg, None, "body_arg defaults to None (last arg)");
        assert_eq!(e.keyword, "struct", "keyword defaults to struct");
    }

    #[test]
    fn macro_define_type_class_keyword_accepted() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_define_type = [{ name = \"EXPORT_CLASS\", keyword = \"class\" }]\n",
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("class keyword must load");
        assert_eq!(cfg.cpp.macro_define_type[0].keyword, "class");
    }

    #[test]
    fn macro_define_type_explicit_fields_round_trip() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_define_type = [{ name = \"DEF3\", name_arg = 1, body_arg = 2, keyword = \"struct\" }]\n",
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("explicit fields must load");
        let e = &cfg.cpp.macro_define_type[0];
        assert_eq!(e.name_arg, 1);
        assert_eq!(e.body_arg, Some(2));
    }

    #[test]
    fn macro_define_type_empty_name_rejected() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_define_type = [{ name = \"\" }]\n",
        )
        .unwrap();
        let err = RootConfig::load(dir.path()).expect_err("empty name must be rejected");
        match err {
            ConfigError::MacroDefineTypeEmptyName => {}
            other => panic!("expected MacroDefineTypeEmptyName, got: {other:?}"),
        }
    }

    #[test]
    fn macro_define_type_invalid_keyword_rejected() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_define_type = [{ name = \"X\", keyword = \"enum\" }]\n",
        )
        .unwrap();
        let err =
            RootConfig::load(dir.path()).expect_err("non-struct/class keyword must be rejected");
        match err {
            ConfigError::MacroDefineTypeKeyword { name, keyword } => {
                assert_eq!(name, "X");
                assert_eq!(keyword, "enum");
            }
            other => panic!("expected MacroDefineTypeKeyword, got: {other:?}"),
        }
    }

    #[test]
    fn macro_define_type_within_list_dedup_by_name() {
        // Two entries with the same name dedup to the first occurrence.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_define_type = [\n  { name = \"EXPORT_STRUCT\", keyword = \"struct\" },\n  { name = \"EXPORT_STRUCT\", keyword = \"class\" },\n]\n",
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("dedup load must succeed");
        assert_eq!(
            cfg.cpp.macro_define_type.len(),
            1,
            "within-list duplicates dedup by name"
        );
        assert_eq!(
            cfg.cpp.macro_define_type[0].keyword, "struct",
            "first occurrence wins"
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
        assert!(cfg.extensions.csharp.is_empty());
        assert!(cfg.extensions.java.is_empty());
    }

    #[test]
    fn extensions_section_absent_yields_default() {
        // Backward compatibility: `.code-graph.toml` files without an
        // `[extensions]` section must load cleanly with empty overrides.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".code-graph.toml"), "[discovery]\n").unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("load without [extensions]");
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
        let (cfg, _root) = RootConfig::load(dir.path()).expect("load valid additive lists");
        assert_eq!(cfg.extensions.lookup_additional(".cu"), Some(Language::Cpp));
        assert_eq!(
            cfg.extensions.lookup_additional(".inl"),
            Some(Language::Cpp)
        );
        assert_eq!(
            cfg.extensions.lookup_additional(".pyx"),
            Some(Language::Python)
        );
        assert_eq!(cfg.extensions.lookup_additional(".rs"), None);
    }

    #[test]
    fn extensions_csharp_additive_lookup_returns_csharp() {
        // Round-trip: a `[extensions].csharp = [".aspx"]` TOML deserializes
        // and `lookup_additional(".aspx")` returns `Some(Language::CSharp)`.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            r#"
[extensions]
csharp = [".aspx"]
"#,
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("load valid csharp additive list");
        assert_eq!(
            cfg.extensions.lookup_additional(".aspx"),
            Some(Language::CSharp)
        );
        assert_eq!(cfg.extensions.csharp, vec![".aspx".to_string()]);
    }

    #[test]
    fn extensions_java_additive_lookup_returns_java() {
        // Round-trip: a `[extensions].java = [".jav"]` TOML deserializes
        // and `lookup_additional(".jav")` returns `Some(Language::Java)`.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            r#"
[extensions]
java = [".jav"]
"#,
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("load valid java additive list");
        assert_eq!(
            cfg.extensions.lookup_additional(".jav"),
            Some(Language::Java)
        );
        assert_eq!(cfg.extensions.java, vec![".jav".to_string()]);
    }

    #[test]
    fn extensions_csharp_java_cross_additive_conflict_errors() {
        // Cross-additive collision between two newly-added languages must
        // surface the same `ExtensionConflict` as any other pair — the
        // `additive_lists` widening from 4 → 6 keeps both new languages in
        // the O(n²) collision scan.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            r#"
[extensions]
csharp = [".x"]
java = [".x"]
"#,
        )
        .unwrap();
        let err = RootConfig::load(dir.path())
            .expect_err("csharp/java cross-additive conflict must error");
        match err {
            ConfigError::ExtensionConflict {
                extension,
                first,
                second,
            } => {
                assert_eq!(extension, ".x");
                assert_eq!(first, "csharp");
                assert_eq!(second, "java");
            }
            other => panic!("expected ExtensionConflict, got: {other:?}"),
        }
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
        let (cfg, _root) = RootConfig::load(dir.path()).expect("load valid disabled list");
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
        let (cfg, _root) = RootConfig::load(dir.path()).expect("load mixed-case entries");
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
        let (cfg, _root) =
            RootConfig::load(dir.path()).expect("disabled vs additive overlap is allowed");
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
        let (cfg, _root) =
            RootConfig::load(dir.path()).expect("empty entries must be dropped, not error");
        assert_eq!(cfg.extensions.cpp, vec![".cu".to_string()]);
        assert!(cfg.extensions.disabled.is_empty());
    }

    // --- ResponseConfig tests ----------------------------------------------

    #[test]
    fn response_config_default_is_100kb() {
        // The chosen default is 102_400 bytes (100 KB). Documented in
        // PaginatedResponseSizeSafety/README.md, Decision 8. Encoded as a
        // public constant so downstream wiring can reference the same
        // value without duplicating the magic number. The literal value
        // (102_400) is invariant — if you need to change it, update the
        // README's Decision 8 first.
        let cfg = RootConfig::default();
        assert_eq!(cfg.response.max_bytes, DEFAULT_RESPONSE_MAX_BYTES);
    }

    #[test]
    fn response_section_absent_uses_default() {
        // Backward compatibility: every existing `.code-graph.toml` in the
        // wild has no `[response]` section. Loading must produce the
        // default `max_bytes` with no error.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[discovery]\nmax_threads = 0\n",
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path())
            .expect("config without [response] section must load cleanly");
        assert_eq!(cfg.response.max_bytes, DEFAULT_RESPONSE_MAX_BYTES);
    }

    #[test]
    fn response_section_empty_uses_default() {
        // `[response]` header present but no `max_bytes` key must still
        // yield the default via the `#[serde(default = ...)]` on the field.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".code-graph.toml"), "[response]\n").unwrap();
        let (cfg, _root) = RootConfig::load(dir.path())
            .expect("empty [response] section must yield default max_bytes");
        assert_eq!(cfg.response.max_bytes, DEFAULT_RESPONSE_MAX_BYTES);
    }

    #[test]
    fn response_explicit_override_is_honored() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[response]\nmax_bytes = 51200\n",
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("explicit override must load");
        assert_eq!(cfg.response.max_bytes, 51_200);
    }

    #[test]
    fn response_max_bytes_zero_is_rejected() {
        // A zero budget would make every paginated handler return an empty
        // page with `truncated=true` — silently-broken. The custom
        // deserializer rejects it at load time with a clear error message.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[response]\nmax_bytes = 0\n",
        )
        .unwrap();
        let err = RootConfig::load(dir.path())
            .expect_err("max_bytes = 0 must be rejected, not silently accepted");
        match err {
            ConfigError::Toml(ref e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("max_bytes") && msg.contains("> 0"),
                    "error must clearly name the field and the constraint, got: {msg}"
                );
            }
            other => panic!("expected ConfigError::Toml, got: {other:?}"),
        }
    }

    #[test]
    fn response_max_bytes_negative_is_rejected() {
        // `usize` cannot represent a negative value; `toml`/`serde` reject
        // at the type-coercion layer with its own diagnostic. We verify
        // here that load returns `Err`, not silently coerces or defaults.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[response]\nmax_bytes = -1\n",
        )
        .unwrap();
        let err = RootConfig::load(dir.path())
            .expect_err("negative max_bytes must be rejected, not coerced");
        match err {
            ConfigError::Toml(_) => {}
            other => panic!("expected ConfigError::Toml for negative max_bytes, got: {other:?}"),
        }
    }

    #[test]
    fn response_max_bytes_non_integer_is_rejected() {
        // A floating-point or string value must error at the type-coercion
        // layer rather than silently converting.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[response]\nmax_bytes = 1.5\n",
        )
        .unwrap();
        let err = RootConfig::load(dir.path())
            .expect_err("non-integer max_bytes must be rejected, not coerced");
        match err {
            ConfigError::Toml(_) => {}
            other => panic!("expected ConfigError::Toml for non-integer max_bytes, got: {other:?}"),
        }
    }

    #[test]
    fn response_section_round_trip_serialize_deserialize() {
        // Confirms the section participates in serde round-tripping
        // alongside the other sections. The Serialize derive (no custom
        // ser) emits the field directly; deserialization runs through the
        // custom validator on the way back.
        let mut cfg = RootConfig::default();
        cfg.response.max_bytes = 65_536;
        let serialized = toml::to_string(&cfg).unwrap();
        let back: RootConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(back.response.max_bytes, 65_536);
    }

    #[test]
    fn response_section_coexists_with_other_sections() {
        // Smoke test: every section appearing together still parses and
        // each retains its own values. Confirms the new field doesn't
        // disturb existing section ordering.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            r#"
[discovery]
max_threads = 2
[parsing]
max_threads = 4
[cpp]
macro_strip = ["CORE_API"]
[extensions]
cpp = [".cu"]
[response]
max_bytes = 200000
"#,
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("multi-section config must load");
        assert_eq!(cfg.discovery.max_threads, 2);
        assert_eq!(cfg.parsing.max_threads, 4);
        assert_eq!(cfg.cpp.macro_strip, vec!["CORE_API".to_string()]);
        assert_eq!(cfg.extensions.cpp, vec![".cu".to_string()]);
        assert_eq!(cfg.response.max_bytes, 200_000);
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
        let (cfg, _root) = RootConfig::load(dir.path()).expect("load must succeed");
        assert_eq!(
            cfg.cpp.macro_strip,
            vec!["B".to_string(), "A".to_string(), "C".to_string()],
            "macro_strip must preserve user-listed order"
        );
    }

    // --- CppConfig macro_strip_with_args tests -----------------------------

    #[test]
    fn cpp_macro_strip_with_args_default_empty() {
        // Zero-config users see an empty `macro_strip_with_args` list. Two
        // paths: a TOML with no `[cpp]` section, and an explicit empty array.
        // Both must yield an empty Vec.
        let cfg = RootConfig::default();
        assert!(
            cfg.cpp.macro_strip_with_args.is_empty(),
            "default macro_strip_with_args must be empty (opt-in)"
        );

        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[discovery]\nmax_threads = 0\n",
        )
        .unwrap();
        let (cfg, _root) =
            RootConfig::load(dir.path()).expect("config without [cpp] section must load cleanly");
        assert!(
            cfg.cpp.macro_strip_with_args.is_empty(),
            "absent [cpp] section must default to empty macro_strip_with_args"
        );

        // Explicit `[cpp]\nmacro_strip_with_args = []` is the same.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip_with_args = []\n",
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path())
            .expect("[cpp] with explicit empty macro_strip_with_args must load cleanly");
        assert!(
            cfg.cpp.macro_strip_with_args.is_empty(),
            "explicit empty array must yield empty macro_strip_with_args"
        );
    }

    #[test]
    fn cpp_macro_strip_with_args_round_trips() {
        // The canonical Unreal Engine reflection macros round-trip verbatim.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip_with_args = [\"UCLASS\", \"UFUNCTION\"]\n",
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("load must succeed");
        assert_eq!(
            cfg.cpp.macro_strip_with_args,
            vec!["UCLASS".to_string(), "UFUNCTION".to_string()],
            "macro_strip_with_args must round-trip verbatim"
        );
    }

    #[test]
    fn cpp_macro_strip_with_args_filters_empty_strings() {
        // Mirror of `cpp_macro_strip_filters_empty_strings`: the empty-pattern
        // infinite-loop risk applies equally to the args variant, so the
        // load-time drain is the only safe enforcement point.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip_with_args = [\"\", \"UCLASS\", \"\"]\n",
        )
        .unwrap();
        let (cfg, _root) =
            RootConfig::load(dir.path()).expect("load must succeed even with empty entries");
        assert_eq!(
            cfg.cpp.macro_strip_with_args,
            vec!["UCLASS".to_string()],
            "empty entries must be drained, leaving only valid patterns"
        );
    }

    #[test]
    fn cpp_macro_strip_with_args_dedups_within_list() {
        // Within-list duplicates are silently deduplicated; first-occurrence
        // order is preserved. This is the principle-of-least-surprise: paste
        // mistakes shouldn't error, and the substitution scan is idempotent.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip_with_args = [\"UCLASS\", \"UCLASS\", \"UFUNCTION\"]\n",
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("load must succeed with duplicates");
        assert_eq!(
            cfg.cpp.macro_strip_with_args,
            vec!["UCLASS".to_string(), "UFUNCTION".to_string()],
            "duplicates must be removed AND first-occurrence order preserved"
        );
    }

    #[test]
    fn cpp_macro_strip_conflict_rejected() {
        // The same token in BOTH lists is ambiguous (each list applies a
        // different substitution rule), so the load fails with the specific
        // `MacroStripConflict` variant. Asserting the variant — not just
        // `is_err()` — pins the handler-side error-mapping contract.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip = [\"X\"]\nmacro_strip_with_args = [\"X\"]\n",
        )
        .unwrap();
        let err = RootConfig::load(dir.path())
            .expect_err("token in both lists must produce MacroStripConflict");
        assert!(
            matches!(&err, ConfigError::MacroStripConflict { token } if token == "X"),
            "expected ConfigError::MacroStripConflict {{ token: \"X\" }}, got: {err:?}"
        );
    }

    #[test]
    fn cpp_macro_strip_disjoint_lists_pass() {
        // Distinct tokens in each list: the typical UE case (an API-export
        // macro in `macro_strip` and a reflection macro in
        // `macro_strip_with_args`). Both lists must survive verbatim.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip = [\"ENGINE_API\"]\nmacro_strip_with_args = [\"UCLASS\"]\n",
        )
        .unwrap();
        let (cfg, _root) = RootConfig::load(dir.path()).expect("disjoint lists must load cleanly");
        assert_eq!(cfg.cpp.macro_strip, vec!["ENGINE_API".to_string()]);
        assert_eq!(cfg.cpp.macro_strip_with_args, vec!["UCLASS".to_string()]);
    }

    #[test]
    fn cpp_macro_strip_case_sensitivity_preserved() {
        // `UCLASS` and `uclass` are DISTINCT tokens — C++ macros are
        // case-sensitive. This test pins the invariant against any future
        // "helpful" lowercasing that would silently merge the two and either
        // (a) trigger a false-positive conflict, or (b) corrupt the user's
        // config.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[cpp]\nmacro_strip = [\"UCLASS\"]\nmacro_strip_with_args = [\"uclass\"]\n",
        )
        .unwrap();
        let (cfg, _root) =
            RootConfig::load(dir.path()).expect("case-mismatched tokens must NOT be a conflict");
        assert_eq!(
            cfg.cpp.macro_strip,
            vec!["UCLASS".to_string()],
            "macro_strip entry must survive case-unchanged"
        );
        assert_eq!(
            cfg.cpp.macro_strip_with_args,
            vec!["uclass".to_string()],
            "macro_strip_with_args entry must survive case-unchanged"
        );
    }

    // --- Shipped example file -------------------------------------------

    #[test]
    fn shipped_example_file_is_valid_toml_and_deserializes() {
        // The committed `.code-graph.toml.example` is what users copy to
        // `.code-graph.toml`. Every opt-in preset in it is fully commented,
        // so the default-state file must parse as valid TOML AND
        // round-trip into `RootConfig` with no unknown-key surprises. This
        // guards the example against TOML-syntax rot (a stray comment-out,
        // an unbalanced array, a misnamed key) that wouldn't be caught by
        // any other test, since nothing else loads this file.
        let raw = include_str!("../../../.code-graph.toml.example");
        let cfg: RootConfig = toml::from_str(raw)
            .expect("shipped .code-graph.toml.example must parse as valid RootConfig");
        // With every preset commented out, the example must yield exactly
        // the documented active defaults — proving the opt-in blocks really
        // are inert until uncommented.
        assert_eq!(cfg.discovery.max_threads, 0);
        assert!(cfg.discovery.respect_gitignore);
        assert!(!cfg.discovery.follow_symlinks);
        assert_eq!(
            cfg.discovery.extra_ignore,
            vec![
                "build/".to_string(),
                "node_modules/".to_string(),
                "vendor/".to_string(),
            ],
            "the active extra_ignore must be the documented default trio; \
             the UE preset line must stay commented out"
        );
        assert_eq!(cfg.parsing.max_threads, 0);
        assert!(
            cfg.cpp.macro_strip.is_empty(),
            "the UE macro_strip preset must stay commented out"
        );
        assert!(
            cfg.cpp.macro_strip_with_args.is_empty(),
            "the UE macro_strip_with_args preset must stay commented out"
        );
        // The [response] table is active in the example with `max_bytes`
        // commented out, so it must resolve to the documented default.
        // Pins the same "stays inert until uncommented" guard as the
        // cpp/discovery presets above — an accidental small literal
        // (e.g. `max_bytes = 50`) would otherwise pass unnoticed.
        assert_eq!(
            cfg.response.max_bytes, DEFAULT_RESPONSE_MAX_BYTES,
            "response.max_bytes must be the documented default while \
             the example leaves it commented"
        );
    }
}
